#!/usr/bin/env python3
# =============================================================================
# Plik: filter_jsonl.py
# Opis: Filtruje output AI — przepuszcza poprawne linie JSONL.
#       Akceptuje format guard (text+label) i memory/toolcalling (input+output).
# =============================================================================
import sys
import json

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        obj = json.loads(line)
        # Guard format: {"text": "...", "label": 0/1/2}
        if "text" in obj and "label" in obj and obj["label"] in [0, 1, 2]:
            print(json.dumps(obj, ensure_ascii=False))
        # Memory/toolcalling format: {"input": "...", "output": "..."}
        elif "input" in obj and "output" in obj:
            print(json.dumps(obj, ensure_ascii=False))
    except (json.JSONDecodeError, KeyError, TypeError):
        pass
