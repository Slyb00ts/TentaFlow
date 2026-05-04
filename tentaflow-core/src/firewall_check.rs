// =============================================================================
// File: firewall_check.rs — Windows-only Firewall self-check at startup.
// =============================================================================
//
// Po co:
//   Windows nie zawsze pokazuje monit "Allow access" przy pierwszym bind na
//   port (np. apka odpalona przez non-interactive token, binarka niepodpisana,
//   poprzedni cichy "Block" wpis). Bez reguly inbound na 8090 TCP+UDP nikt
//   z LAN nie polaczy sie z dashboardem ani z naszym mesh node.
//
// Co robi:
//   Przy starcie aplikacji (tylko target_os="windows"):
//     1. Sprawdza czy istnieje regula firewall pozwalajaca na inbound na
//        porty 8090 TCP i 8090 UDP dla NASZEJ binarki.
//     2. Jak brak — odpala elevated PowerShell przez UAC ze skryptem
//        New-NetFirewallRule. UAC wyswietla user-friendly monit zezwalajacy
//        na elevation; po jego akceptacji reguly powstaja na stale i
//        kolejne uruchomienia ida bez interakcji.
//     3. Jak user odmowi UAC — kontynuujemy bez bledu (server wystartuje
//        ale moze nie byc widoczny z innych hostow).
//
// Re-entrancy: idempotentne. Multiple wywolania w jednej sesji = ten sam
//   exitcheckpoint po pierwszym powodzeniu.
// =============================================================================

#![cfg(target_os = "windows")]

use std::process::Command;

/// Prefix nazwy reguly firewall (widoczny w `wf.msc` jako "TentaFlow Inbound" /
/// "TentaFlow Outbound").
const RULE_PREFIX: &str = "TentaFlow";

/// Kierunki regul ktore zakladamy. Inbound dla dashboardu/mesh/QUIC, Outbound
/// dla DHT/relay/upstream HTTPS — domyslnie Windows pozwala na outbound, ale
/// niektore antywirusy / Smart App Control go blokuja, wiec dodajemy explicite.
const DIRECTIONS: &[&str] = &["Inbound", "Outbound"];

/// Sprawdza i jesli trzeba zaklada reguly firewall pozwalajace TentaFlow
/// na CALY ruch (wszystkie protokoly, wszystkie porty) per-binarka.
/// "Best effort" — kazdy blad logujemy ale nie przerywamy startu.
pub fn ensure_firewall_rules() {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("firewall_check: nie udalo sie pobrac sciezki binarki: {}", e);
            return;
        }
    };
    let exe_path_str = exe_path.to_string_lossy().to_string();

    let mut missing: Vec<&'static str> = Vec::new();
    for &dir in DIRECTIONS {
        if !rule_exists(&exe_path_str, dir) {
            missing.push(dir);
        }
    }

    if missing.is_empty() {
        tracing::debug!("firewall_check: wszystkie reguly juz istnieja");
        return;
    }

    tracing::info!(
        "firewall_check: brakuje regul firewall: {:?}. Otwieram monit UAC.",
        missing
    );

    if let Err(e) = request_elevated_rules(&exe_path_str, &missing) {
        tracing::warn!(
            "firewall_check: nie udalo sie zalozyc regul firewall ({}). Server wystartuje, \
             ale moze nie byc widoczny z LAN. Aby ustawic recznie uruchom jako Admin: \
             New-NetFirewallRule -DisplayName \"TentaFlow Inbound\" -Direction Inbound \
             -Program \"{}\" -Action Allow",
            e, exe_path_str
        );
    }
}

/// Sprawdza obecnosc reguly Allow per-binarka w danym kierunku.
/// Wymaga PS NetSecurity module (Win 8.1+ — w kazdym wspieranym Windowsie).
fn rule_exists(exe_path: &str, direction: &str) -> bool {
    let exe_escaped = exe_path.replace('\'', "''");

    let script = format!(
        "$rules = Get-NetFirewallRule -Direction {dir} -Action Allow -Enabled True \
            -ErrorAction SilentlyContinue; \
         foreach ($r in $rules) {{ \
            $app = $r | Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue; \
            if ($app -and $app.Program -ieq '{exe}') {{ exit 0 }} \
         }} \
         exit 1",
        dir = direction,
        exe = exe_escaped
    );

    let output = Command::new("powershell.exe")
        .args(&[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Odpala elevated PowerShell ze skryptem dodajacym brakujace reguly.
/// `Start-Process -Verb RunAs` w PS triggeruje UAC; jak user odmowi,
/// powershell zwraca non-zero exit.
fn request_elevated_rules(
    exe_path: &str,
    missing: &[&'static str],
) -> std::io::Result<()> {
    let exe_escaped = exe_path.replace('\'', "''");

    // Sciezka skryptu robocza — w TEMP zeby uniknac problemow z permissions.
    let mut script_path = std::env::temp_dir();
    script_path.push(format!("tentaflow-firewall-{}.ps1", std::process::id()));

    // Generujemy plik PS z wszystkimi New-NetFirewallRule, jeden per kierunek.
    // Per-binarka, wszystkie protokoly, wszystkie porty — najszerszy mozliwy
    // allow zeby Windows Firewall NIE byl podejrzanym dla zadnego ruchu
    // tentaflow.exe (TCP, UDP, ephemeral DHT, mDNS, QUIC, dashboard itd.).
    let mut script = String::from(
        "# Auto-generated by tentaflow firewall_check.\n\
         $ErrorActionPreference = 'Stop'\n",
    );
    for direction in missing {
        script.push_str(&format!(
            "Write-Host 'Dodaje regule {prefix} {dir}'\n\
             New-NetFirewallRule -DisplayName '{prefix} {dir}' \
                -Direction {dir} -Action Allow \
                -Program '{exe}' -Profile Any -Enabled True | Out-Null\n",
            prefix = RULE_PREFIX,
            dir = direction,
            exe = exe_escaped
        ));
    }

    std::fs::write(&script_path, &script)?;

    // -Verb RunAs w Start-Process trigguje UAC. Cale wywolanie owijamy w
    // outer powershell ktora czeka az inner skonczy.
    let inner = format!(
        "Start-Process powershell.exe -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','{}' -Verb RunAs -Wait",
        script_path.to_string_lossy().replace('\'', "''")
    );

    let status = Command::new("powershell.exe")
        .args(&[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &inner,
        ])
        .status();

    let _ = std::fs::remove_file(&script_path);

    match status {
        Ok(s) if s.success() => {
            tracing::info!("firewall_check: reguly dodane. UAC zaakceptowany.");
            Ok(())
        }
        Ok(s) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("UAC odrzucony lub elevated PS zwrocil exit code {:?}", s.code()),
        )),
        Err(e) => Err(e),
    }
}
