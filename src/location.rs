//! CoreLocation: authorization and a single location fix.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_core_location::{
    CLAuthorizationStatus, CLError, CLLocation, CLLocationManager, CLLocationManagerDelegate,
    kCLLocationAccuracyBest,
};
use objc2_foundation::{
    NSArray, NSDate, NSDefaultRunLoopMode, NSError, NSObject, NSObjectProtocol, NSRunLoop,
};

use crate::Failure;
use crate::ui::Progress;

/// How long to wait for the user to answer the permission prompt. Kept
/// separate from --timeout so a first run doesn't expire while the dialog is up.
const AUTH_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to wait for locationd to report a new manager's real authorization
/// status (it says NotDetermined until registration settles, usually milliseconds).
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(2);
pub const SETTINGS_PATH: &str = "System Settings > Privacy & Security > Location Services";
pub const SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_LocationServices";

#[derive(Default)]
struct DelegateState {
    auth_changed: Cell<bool>,
    location: RefCell<Option<Retained<CLLocation>>>,
    error: RefCell<Option<(isize, String)>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and Delegate doesn't implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateState]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl CLLocationManagerDelegate for Delegate {
        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn did_change_authorization(&self, _manager: &CLLocationManager) {
            self.ivars().auth_changed.set(true);
        }

        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update_locations(
            &self,
            _manager: &CLLocationManager,
            locations: &NSArray<CLLocation>,
        ) {
            if let Some(loc) = locations.lastObject() {
                *self.ivars().location.borrow_mut() = Some(loc);
            }
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        fn did_fail(&self, _manager: &CLLocationManager, error: &NSError) {
            *self.ivars().error.borrow_mut() =
                Some((error.code(), error.localizedDescription().to_string()));
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateState::default());
        unsafe { msg_send![super(this), init] }
    }
}

/// Run the main run loop in short slices until `done` returns true or `limit`
/// elapses, ticking the progress line on each slice.
pub fn pump_until(limit: Duration, progress: &mut Progress, mut done: impl FnMut() -> bool) -> bool {
    let run_loop = NSRunLoop::currentRunLoop();
    let deadline = Instant::now() + limit;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let slice = NSDate::dateWithTimeIntervalSinceNow(remaining.as_secs_f64().min(0.1));
        run_loop.runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &slice);
        progress.tick();
    }
    true
}

/// Create a manager and wait for locationd to report its real authorization status.
fn registered_manager(
    mtm: MainThreadMarker,
    progress: &mut Progress,
) -> (Retained<CLLocationManager>, Retained<Delegate>) {
    let delegate = Delegate::new(mtm);
    let manager = unsafe { CLLocationManager::new() };
    unsafe { manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    // The first callback can still say NotDetermined (e.g. right after a re-sign),
    // so wait for a definite answer; a genuinely undecided client costs the full wait.
    pump_until(REGISTRATION_TIMEOUT, progress, || {
        delegate.ivars().auth_changed.get()
            && unsafe { manager.authorizationStatus() } != CLAuthorizationStatus::NotDetermined
    });
    (manager, delegate)
}

fn services_enabled() -> bool {
    unsafe { CLLocationManager::locationServicesEnabled_class() }
}

fn denied_failure(status: CLAuthorizationStatus) -> Failure {
    if status == CLAuthorizationStatus::Restricted {
        Failure::new(
            "restricted",
            3,
            "Location access is restricted on this Mac (e.g. by a configuration profile or Screen Time).",
        )
    } else {
        Failure::new(
            "denied",
            3,
            format!("Location access was denied. Enable whereami in {SETTINGS_PATH}, then run it again."),
        )
        .with_settings_link()
    }
}

pub fn get_location(
    mtm: MainThreadMarker,
    timeout: Duration,
    progress: &mut Progress,
) -> Result<Retained<CLLocation>, Failure> {
    if !services_enabled() {
        return Err(Failure::new(
            "location_services_disabled",
            4,
            format!("Location Services are turned off. Turn them on in {SETTINGS_PATH}."),
        )
        .with_settings_link());
    }

    progress.set("Locating…");
    let (manager, delegate) = registered_manager(mtm, progress);
    let state = delegate.ivars();
    unsafe { manager.setDesiredAccuracy(kCLLocationAccuracyBest) };

    let mut status = unsafe { manager.authorizationStatus() };
    if status == CLAuthorizationStatus::NotDetermined {
        progress.set("Waiting for permission — answer the macOS dialog…");
        state.auth_changed.set(false);
        unsafe { manager.requestWhenInUseAuthorization() };
        let answered = pump_until(AUTH_TIMEOUT, progress, || {
            state.auth_changed.get()
                && unsafe { manager.authorizationStatus() } != CLAuthorizationStatus::NotDetermined
        });
        if !answered {
            return Err(Failure::new(
                "authorization_timeout",
                5,
                format!(
                    "No answer to the location permission prompt within {}s.",
                    AUTH_TIMEOUT.as_secs()
                ),
            ));
        }
        status = unsafe { manager.authorizationStatus() };
        progress.set("Locating…");
    }
    if status == CLAuthorizationStatus::Denied || status == CLAuthorizationStatus::Restricted {
        return Err(denied_failure(status));
    }

    unsafe { manager.requestLocation() };
    let mut failure = None;
    let finished = pump_until(timeout, progress, || {
        if state.location.borrow().is_some() {
            return true;
        }
        if let Some((code, message)) = state.error.borrow_mut().take() {
            // LocationUnknown is transient: CoreLocation keeps trying.
            if code != CLError::LocationUnknown.0 {
                failure = Some(if code == CLError::Denied.0 {
                    denied_failure(unsafe { manager.authorizationStatus() })
                } else {
                    Failure::new("location_error", 1, format!("{message} (CLError {code})"))
                });
                return true;
            }
        }
        false
    });
    unsafe { manager.stopUpdatingLocation() };
    if let Some(f) = failure {
        return Err(f);
    }
    if !finished {
        return Err(Failure::new(
            "timeout",
            5,
            format!("No location fix within {}s.", timeout.as_secs_f64()),
        ));
    }
    Ok(state.location.borrow_mut().take().expect("location set"))
}

fn status_name(status: CLAuthorizationStatus) -> &'static str {
    match status {
        CLAuthorizationStatus::NotDetermined => "not_determined",
        CLAuthorizationStatus::Restricted => "restricted",
        CLAuthorizationStatus::Denied => "denied",
        CLAuthorizationStatus::AuthorizedAlways => "authorized",
        CLAuthorizationStatus::AuthorizedWhenInUse => "authorized_when_in_use",
        _ => "unknown",
    }
}

/// (location services enabled, authorization status name), without requesting anything.
pub fn status(mtm: MainThreadMarker, progress: &mut Progress) -> (bool, &'static str) {
    let (manager, _delegate) = registered_manager(mtm, progress);
    (services_enabled(), status_name(unsafe { manager.authorizationStatus() }))
}
