// ===== File: windows_host.rs — the server as a Windows service (Service Control Manager) =====
//
// install.ps1 registers `tentaflow.exe --windows-service ...` as the TentaFlow
// service. The Service Control Manager starts that process and expects it to
// connect back through StartServiceCtrlDispatcher within 30 seconds, report
// RUNNING, and answer STOP / SHUTDOWN. Everything else is the ordinary server:
// a stop request resolves the same future Ctrl+C resolves in a console, so the
// graceful shutdown path (databases, mesh, child services) is one and the same.

use std::ffi::OsString;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::Notify;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::Args;

/// The name install.ps1 registers and `tentaflow start|stop|status` address.
pub const SERVICE_NAME: &str = "TentaFlow";

/// The dispatcher calls the service entry point with its own argument vector
/// (the service start parameters), not the process command line, so the parsed
/// command line waits here until the entry point takes it.
static ARGS: Mutex<Option<Args>> = Mutex::new(None);

/// Resolved by a STOP or SHUTDOWN control; the server awaits it next to Ctrl+C.
fn stop_signal() -> &'static Notify {
    static STOP: OnceLock<Notify> = OnceLock::new();
    STOP.get_or_init(Notify::new)
}

/// Completes when the Service Control Manager asks the service to stop. Never
/// completes in a process that is not a service.
pub async fn stop_requested() {
    stop_signal().notified().await;
}

define_windows_service!(ffi_service_main, service_main);

/// Hands the process to the Service Control Manager. Returns once the service
/// has stopped. Started by hand from a console it fails at once: only the SCM
/// can connect a process to the dispatcher.
pub fn run(args: Args) -> Result<()> {
    *ARGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(args);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).context(
        "--windows-service is how the Service Control Manager starts TentaFlow; \
         to run the server in this console, start it without that flag",
    )
}

fn service_main(_arguments: Vec<OsString>) {
    // There is no console and no journal behind a service: whatever fails here
    // is reported to the SCM as the exit code and, once logging is up, in the
    // log file under <home>\logs.
    if let Err(err) = run_service() {
        tracing::error!("TentaFlow service ended with an error: {err:#}");
    }
}

fn run_service() -> Result<()> {
    let args = ARGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or_else(|| anyhow!("service started without command-line arguments"))?;

    let handle = service_control_handler::register(SERVICE_NAME, |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            // notify_one stores a permit, so a stop that lands before the
            // server reaches its await is not lost.
            stop_signal().notify_one();
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .context("registering the service control handler")?;

    let report = |state: ServiceState, accept: ServiceControlAccept, exit: ServiceExitCode| {
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: exit,
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })
    };

    // RUNNING as soon as the handler exists: the SCM measures the time to this
    // report, and the server's own startup (database migrations, model and
    // addon loading) can take far longer than its 30 s budget.
    report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        ServiceExitCode::Win32(0),
    )
    .context("reporting RUNNING")?;

    let result = crate::serve(args);

    // A service-specific exit code tells the SCM the stop was a failure, which
    // is what triggers the recovery actions install.ps1 configures.
    let exit = if result.is_ok() {
        ServiceExitCode::Win32(0)
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };
    report(ServiceState::Stopped, ServiceControlAccept::empty(), exit)
        .context("reporting STOPPED")?;
    result
}
