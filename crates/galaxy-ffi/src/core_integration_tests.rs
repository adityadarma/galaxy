//! Core command, listener, and document opening integration regressions.

use super::*;
use std::sync::mpsc;

struct ChannelListener(Mutex<mpsc::Sender<CoreEvent>>);

impl CoreListener for ChannelListener {
    fn on_event(&self, event: CoreEvent) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

#[test]
fn command_palette_searches_builtin_commands() {
    let p = CommandPalette::new();
    assert!(p.commands().len() > 30);
    let found = p.search("save as".into(), None);
    assert_eq!(found[0].id, "workbench.action.files.saveAs");
    assert_eq!(found[0].label, "File: Save As…");
    assert!(!found[0].highlights.is_empty());
    p.note_used("workbench.action.gotoLine".into());
    let only = p.search(
        String::new(),
        Some(vec!["workbench.action.gotoLine".into()]),
    );
    assert_eq!(only.len(), 1);
    assert!(only[0].is_recent);
    assert_eq!(p.recent(), ["workbench.action.gotoLine"]);
}

#[test]
fn ping_delivers_pong_via_listener() {
    let (tx, rx) = mpsc::channel();
    let core = Core::new();
    core.set_listener(Arc::new(ChannelListener(Mutex::new(tx))));
    core.ping("hi".into());
    let event = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
    assert_eq!(
        event,
        CoreEvent::Pong {
            message: "hi".into()
        }
    );
}

#[test]
fn open_missing_document_errors() {
    let err = Core::new().open_document("/nonexistent/x".into());
    assert!(matches!(err, Err(CoreError::Io { .. })));
}
