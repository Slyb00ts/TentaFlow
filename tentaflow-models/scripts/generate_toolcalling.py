#!/usr/bin/env python3
# =============================================================================
# Plik: generate_toolcalling.py
# Opis: Automatycznie generuje dane treningowe dla tool calling na podstawie
#       manifest.toml i SKILL.md z addonow TentaFlow.
# Uzycie:
#   python3 scripts/generate_toolcalling.py              — generuj z Claude
#   python3 scripts/generate_toolcalling.py --dry-run    — pokaz prompt bez generowania
#   python3 scripts/generate_toolcalling.py --iterations 20
# =============================================================================
import argparse
import json
import os
import subprocess
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
CORE = os.path.normpath(os.path.join(ROOT, "..", "tentaflow-core"))
ADDONS_DIR = os.path.join(CORE, "addons")
OUTPUT_FILE = os.path.join(ROOT, "data", "toolcalling", "raw.jsonl")
FILTER = os.path.join(ROOT, "scripts", "filter_jsonl.py")

# Addony do pominiecia (testowe, zlosliwe)
SKIP_ADDONS = {"test-addon", "malicious-addon"}


def parse_manifest(addon_dir):
    """Parsuje manifest.toml addona — wyciaga toole z opisami i parametrami."""
    import tomllib
    manifest_path = os.path.join(addon_dir, "manifest.toml")
    if not os.path.exists(manifest_path):
        return None

    with open(manifest_path, "rb") as f:
        data = tomllib.load(f)

    addon = data.get("addon", {})
    addon_id = addon.get("id", "")
    if addon_id in SKIP_ADDONS:
        return None

    tools = []
    tools_section = data.get("tools", {})
    for tool_name, tool_data in tools_section.items():
        if isinstance(tool_data, dict):
            desc = tool_data.get("description", "")
            keywords = tool_data.get("keywords", [])
            params = tool_data.get("parameters", {})
            properties = params.get("properties", {})
            required = params.get("required", [])

            params_list = []
            for pname, pdata in properties.items():
                if isinstance(pdata, dict):
                    is_req = pname in required
                    pdesc = pdata.get("description", "")
                    ptype = pdata.get("type", "string")
                    params_list.append({
                        "name": pname,
                        "type": ptype,
                        "description": pdesc,
                        "required": is_req,
                    })

            tools.append({
                "addon_id": addon_id,
                "tool_name": tool_name,
                "full_name": f"{addon_id}.{tool_name}",
                "description": desc,
                "keywords": keywords,
                "params": params_list,
            })

    return {
        "addon_id": addon_id,
        "name": addon.get("name", addon_id),
        "description": addon.get("description", ""),
        "category": addon.get("category", ""),
        "keywords": addon.get("keywords", []),
        "tools": tools,
    }


def read_skill(addon_dir):
    """Czyta SKILL.md jesli istnieje."""
    skill_path = os.path.join(addon_dir, "SKILL.md")
    if os.path.exists(skill_path):
        with open(skill_path, "r", encoding="utf-8") as f:
            return f.read()
    return ""


def build_tools_compact(addons):
    """Buduje kompaktowa liste tooli do inputu modelu (format z TOON)."""
    lines = []
    for addon in addons:
        for tool in addon["tools"]:
            params = []
            for p in tool["params"]:
                suffix = "*" if p["required"] else "?"
                params.append(f"{p['name']}{suffix}")
            kw = tool.get("keywords", [])
            kw_str = f" [{','.join(kw[:5])}]" if kw else ""
            lines.append(f"{tool['full_name']}({','.join(params)}){kw_str}")
    return "\n".join(lines)


def build_prompt(addons, tools_compact):
    """Buduje prompt dla Claude do generowania danych toolcalling."""

    # Szczegolowy opis tooli
    tools_detail = ""
    for addon in addons:
        tools_detail += f"\n### {addon['name']} ({addon['addon_id']})\n"
        tools_detail += f"{addon['description']}\n"
        for tool in addon["tools"]:
            req_params = [p for p in tool["params"] if p["required"]]
            opt_params = [p for p in tool["params"] if not p["required"]]
            tools_detail += f"\n**{tool['full_name']}** — {tool['description']}\n"
            if req_params:
                tools_detail += f"  Required: {', '.join(p['name'] + ' (' + p['description'] + ')' for p in req_params)}\n"
            if opt_params:
                tools_detail += f"  Optional: {', '.join(p['name'] + ' (' + p['description'] + ')' for p in opt_params)}\n"

    prompt = f"""KRYTYCZNE: Twoj JEDYNY output to surowe linie JSONL. ZERO komentarzy, ZERO markdown, ZERO tabel, ZERO opisow. Pierwszym znakiem outputu MUSI byc `{{`. Jesli wygenerujesz cokolwiek innego niz linie JSONL — zadanie jest NIEUDANE.

Generujesz dane treningowe JSONL. Kazda linia to JSON: {{"input": "...", "output": "..."}}

Wygeneruj **20 linii JSONL** (8 PL, 6 EN, 6 Inne: DE, FR, ES, RU, ZH, JA).

Dostepne narzedzia:
{tools_compact}

## FORMAT TOON (output modelu)

- Single tool: `@addon.function|param1=value1|param2=value2`
- Multi tool chain: kazdy na osobnej linii
- Missing params: `#MISSING@addon.function|known=val|missing=?`
- No tool needed: `#TEXT\\nOdpowiedz tekstowa`

## PROPORCJE

- 40% single tool call — user podaje wystarczajaco danych
- 15% missing params — user nie podaje wszystkich required params
- 15% multi tool chain — user chce kilka rzeczy naraz (sekwencyjne)
- 15% no tool needed (#TEXT) — pytanie nie wymaga narzedzia
- 5% narzedzie niedostepne (#TEXT — user prosi o cos czego nie ma)
- 10% niejednoznaczne zapytania — moze pasowac wiecej niz jedno narzedzie

## REGULY

1. Required params MUSZA miec wartosc (lub `?` jesli #MISSING)
2. Optional params — TYLKO gdy user je podal, nie dodawaj na zapas
3. Multi-chain: uzywaj gdy zapytanie wymaga SEKWENCYJNYCH akcji
4. #TEXT: pytanie o wiedze, small talk, lub brak odpowiedniego narzedzia
5. Rozne style: formalny, nieformalny, techniczny, skrotowy
6. Rozne dlugosci: od "Pokaz maile" do 3-4 zdan z kontekstem
7. KAZDY rekord ma w input pelna liste tooli w formacie kompaktowym

## DISAMBIGUATION
- "napisz do X" bez kontekstu → domyslnie email (outlook.send_email)
- "napisz na Teams/Slack/czat" → teams.send_message
- "znajdz plik/dokument" → sharepoint-rag.search_files
- "znajdz maila" → outlook.search_emails
- "sprawdz kalendarz" → teams.get_calendar

WAZNE: W kazdym rekordzie pole "input" MUSI zaczynac sie od "<|tools|>\\n" a potem pelna lista tooli, potem "\\n<|query|>\\n" i zapytanie.

Przyklady outputu (input pomijam bo zawsze ten sam format):
- query "Wyslij Kasi maila" → output "@outlook.send_email|to=kasia|subject=Info|body=Czesc Kasiu"
- query "Nowe maile?" → output "@outlook.list_emails|filter=isRead eq false"
- query "Co to REST?" → output "#TEXT\\nREST to architektura API oparta na HTTP."
- query "Wyslij maila ale nie wiem do kogo" → output "#MISSING@outlook.send_email|to=?|subject=?|body=?"
- query "Znajdz raport i wyslij Markowi" → output "@sharepoint-rag.search_files|query=raport\\n@teams.send_message|to=marek|message=Raport"
"""

    return prompt


def generate_batch(prompt):
    """Wywoluje Claude CLI z promptem i filtruje output."""
    # Zapisz prompt do pliku tymczasowego — unika problemow z limitem stdin
    import tempfile
    with tempfile.NamedTemporaryFile(mode='w', suffix='.md', delete=False, encoding='utf-8') as f:
        f.write(prompt)
        tmp_path = f.name

    try:
        result = subprocess.run(
            ["claude", "-p", "--dangerously-skip-permissions"],
            stdin=open(tmp_path, 'r'),
            capture_output=True,
            text=True,
            timeout=300,
        )
        output = result.stdout.strip()
    except subprocess.TimeoutExpired:
        print("    TIMEOUT — pomijam batch")
        return []
    finally:
        os.unlink(tmp_path)

    # Filtruj valid JSONL
    valid = []
    for line in output.split("\n"):
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
            if "input" in obj and "output" in obj:
                valid.append(obj)
        except json.JSONDecodeError:
            pass
    return valid


def main():
    parser = argparse.ArgumentParser(description="Generate toolcalling training data from addons")
    parser.add_argument("--iterations", type=int, default=5, help="Number of batches to generate")
    parser.add_argument("--dry-run", action="store_true", help="Print prompt without generating")
    args = parser.parse_args()

    # Skanuj addony
    print("Skanowanie addonow...")
    addons = []
    for name in sorted(os.listdir(ADDONS_DIR)):
        addon_dir = os.path.join(ADDONS_DIR, name)
        if not os.path.isdir(addon_dir):
            continue
        manifest = parse_manifest(addon_dir)
        if manifest and manifest["tools"]:
            skill = read_skill(addon_dir)
            manifest["skill_md"] = skill
            addons.append(manifest)
            tool_count = len(manifest["tools"])
            print(f"  {manifest['addon_id']}: {tool_count} tooli")

    total_tools = sum(len(a["tools"]) for a in addons)
    print(f"\nLacznie: {len(addons)} addonow, {total_tools} tooli")

    # Buduj prompt
    tools_compact = build_tools_compact(addons)
    prompt = build_prompt(addons, tools_compact)

    if args.dry_run:
        print("\n" + "=" * 60)
        print("PROMPT (dry-run):")
        print("=" * 60)
        print(prompt)
        print(f"\nTools compact ({len(tools_compact)} chars):")
        print(tools_compact)
        return

    # Generuj
    os.makedirs(os.path.dirname(OUTPUT_FILE), exist_ok=True)
    existing = 0
    if os.path.exists(OUTPUT_FILE):
        with open(OUTPUT_FILE) as f:
            existing = sum(1 for _ in f)

    print(f"\nGenerowanie {args.iterations} batchy...")
    print(f"Output: {OUTPUT_FILE} (istniejace: {existing})")

    total_added = 0
    for i in range(args.iterations):
        records = generate_batch(prompt)
        if records:
            with open(OUTPUT_FILE, "a", encoding="utf-8") as f:
                for r in records:
                    f.write(json.dumps(r, ensure_ascii=False) + "\n")
            total_added += len(records)
        print(f"  [{i+1}/{args.iterations}] +{len(records)} (total: {existing + total_added})")

    print(f"\nGotowe: +{total_added} rekordow (lacznie: {existing + total_added})")


if __name__ == "__main__":
    main()
