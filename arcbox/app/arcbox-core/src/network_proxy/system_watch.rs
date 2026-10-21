//! macOS: notices changes to the Mac's proxy settings as they happen.
//!
//! `configd` rewrites one dynamic-store key whenever the effective proxy
//! settings change (network service switch, VPN up or down, an edit in
//! System Settings), so watching it replaces polling `scutil --proxy`.

use std::io;

use system_configuration::core_foundation::array::CFArray;
use system_configuration::core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use system_configuration::core_foundation::string::CFString;
use system_configuration::dynamic_store::{
    SCDynamicStore, SCDynamicStoreBuilder, SCDynamicStoreCallBackContext,
};

/// The effective, merged proxy settings of the Mac.
const PROXIES_KEY: &str = "State:/Network/Global/Proxies";

type Notify = Box<dyn Fn() + Send>;

/// Starts a thread that calls `on_change` after every change to the Mac's
/// proxy settings. The thread runs its own run loop for the life of the
/// process; returns once the watch is registered.
pub(super) fn watch(on_change: impl Fn() + Send + 'static) -> io::Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("system-proxy-watch".to_owned())
        .spawn(move || {
            let context = SCDynamicStoreCallBackContext {
                callout: changed,
                info: Box::new(on_change) as Notify,
            };
            let Some(store) = SCDynamicStoreBuilder::new("arcbox-proxy-watch")
                .callback_context(context)
                .build()
            else {
                let _ = ready_tx.send(Err("cannot open the dynamic store"));
                return;
            };
            let keys = CFArray::from_CFTypes(&[CFString::from_static_string(PROXIES_KEY)]);
            let patterns: CFArray<CFString> = CFArray::from_CFTypes(&[]);
            if !store.set_notification_keys(&keys, &patterns) {
                let _ = ready_tx.send(Err("cannot watch the proxy settings key"));
                return;
            }
            let Some(source) = store.create_run_loop_source() else {
                let _ = ready_tx.send(Err("cannot create a run loop source"));
                return;
            };
            // SAFETY: `kCFRunLoopCommonModes` is an immutable CoreFoundation
            // constant, valid for the life of the process.
            CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
            let _ = ready_tx.send(Ok(()));
            CFRunLoop::run_current();
        })?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(reason)) => Err(io::Error::other(reason)),
        Err(_) => Err(io::Error::other("the proxy watch thread exited")),
    }
}

#[allow(clippy::needless_pass_by_value)] // the binding fixes the signature
fn changed(_store: SCDynamicStore, _keys: CFArray<CFString>, notify: &mut Notify) {
    notify();
}
