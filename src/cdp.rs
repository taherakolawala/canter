//! The Chrome DevTools Protocol over one WebSocket.
//!
//! Commands carry an id and are answered out of order, so `call` reads
//! messages until the matching id arrives and keeps the events it passes on
//! the way. One socket serves the browser and every page attached to it: a
//! page command carries that page's session id.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ws::{WebSocket, WsError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdpError {
    /// The browser went away: its window was closed, or it exited.
    Closed,
    /// No answer arrived before the caller's deadline.
    Timeout,
    /// The browser answered the command with an error.
    Remote(String),
    /// The browser sent something this client cannot use.
    Protocol(String),
}

impl CdpError {
    pub fn message(&self) -> String {
        match self {
            CdpError::Closed => "The sign-in browser is no longer running.".into(),
            CdpError::Timeout => "The sign-in browser stopped responding.".into(),
            CdpError::Remote(why) => format!("The sign-in browser refused a step: {why}"),
            CdpError::Protocol(why) => why.clone(),
        }
    }
}

pub trait Wire {
    fn send(&mut self, text: &str) -> Result<(), CdpError>;
    /// The next message, or `None` when nothing arrived before the
    /// connection's own read timeout.
    fn recv(&mut self) -> Result<Option<String>, CdpError>;
}

impl<S: Read + Write> Wire for WebSocket<S> {
    fn send(&mut self, text: &str) -> Result<(), CdpError> {
        self.send_text(text).map_err(wire_error)
    }

    fn recv(&mut self) -> Result<Option<String>, CdpError> {
        WebSocket::recv(self).map_err(wire_error)
    }
}

fn wire_error(e: WsError) -> CdpError {
    match e {
        WsError::Closed => CdpError::Closed,
        WsError::Failed(why) => CdpError::Protocol(why),
    }
}

pub struct Cdp<W: Wire> {
    wire: W,
    next_id: u64,
    /// Events seen while waiting for an answer, counted but not kept: the
    /// sign-in flow reads the page itself rather than following events.
    events: u64,
}

impl<W: Wire> Cdp<W> {
    pub fn new(wire: W) -> Self {
        Cdp {
            wire,
            next_id: 1,
            events: 0,
        }
    }

    /// Send one command and wait for its answer. `session` is `None` for the
    /// browser itself.
    pub fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CdpError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            request["sessionId"] = Value::from(session);
        }
        self.wire.send(&request.to_string())?;
        let deadline = Instant::now() + timeout;
        loop {
            let Some(text) = self.wire.recv()? else {
                if Instant::now() >= deadline {
                    return Err(CdpError::Timeout);
                }
                continue;
            };
            let Ok(message) = serde_json::from_str::<Value>(&text) else {
                return Err(CdpError::Protocol(
                    "The sign-in browser sent an unreadable message.".into(),
                ));
            };
            match message.get("id").and_then(Value::as_u64) {
                Some(answered) if answered == id => {
                    if let Some(error) = message.get("error") {
                        let why = error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error");
                        return Err(CdpError::Remote(why.to_string()));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                // Another command's answer: this client sends one at a time,
                // so it can only be a late answer that no longer matters.
                Some(_) => continue,
                None => self.events = self.events.saturating_add(1),
            }
            if Instant::now() >= deadline {
                return Err(CdpError::Timeout);
            }
        }
    }

    /// How many events arrived while waiting for answers. Only the tests
    /// read this; the flow itself has no use for events.
    #[cfg(test)]
    pub fn events_seen(&self) -> u64 {
        self.events
    }
}

/// How long any single protocol command may take.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// One attached page, addressed by its session id.
pub struct Page<'a, W: Wire> {
    pub cdp: &'a mut Cdp<W>,
    pub session: String,
}

impl<W: Wire> Page<'_, W> {
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, CdpError> {
        let session = self.session.clone();
        self.cdp
            .call(Some(&session), method, params, COMMAND_TIMEOUT)
    }

    /// Evaluate an expression and return its value. The expression must not
    /// contain anything secret: it is a protocol string like any other.
    pub fn evaluate(&mut self, expression: &str) -> Result<Value, CdpError> {
        let result = self.call(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "awaitPromise": false}),
        )?;
        value_of(&result)
    }

    /// Type into whatever the page has focused. The text never appears in an
    /// expression, a log line or a file.
    pub fn insert_text(&mut self, text: &str) -> Result<(), CdpError> {
        self.call("Input.insertText", json!({"text": text}))
            .map(|_| ())
    }

    pub fn navigate(&mut self, url: &str) -> Result<(), CdpError> {
        let result = self.call("Page.navigate", json!({"url": url}))?;
        match result.get("errorText").and_then(Value::as_str) {
            Some(_) => Err(CdpError::Protocol(
                "The sign-in page could not be loaded. Check your network connection.".into(),
            )),
            None => Ok(()),
        }
    }
}

/// The value of a `Runtime.evaluate` result, or the page's own exception.
pub fn value_of(result: &Value) -> Result<Value, CdpError> {
    if result.get("exceptionDetails").is_some() {
        return Err(CdpError::Protocol(
            "The sign-in page did not answer as expected.".into(),
        ));
    }
    Ok(result
        .pointer("/result/value")
        .cloned()
        .unwrap_or(Value::Null))
}

/// The id of the first page target, used to attach to the sign-in window.
pub fn first_page_target(targets: &Value) -> Option<String> {
    targets
        .get("targetInfos")?
        .as_array()?
        .iter()
        .find(|info| info.get("type").and_then(Value::as_str) == Some("page"))
        .and_then(|info| info.get("targetId").and_then(Value::as_str))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted connection: records what was sent and replays answers.
    /// `None` is a read timeout.
    struct FakeWire {
        sent: Vec<String>,
        replies: VecDeque<Option<String>>,
        closed: bool,
    }

    impl FakeWire {
        fn new(replies: Vec<Option<&str>>) -> Self {
            FakeWire {
                sent: Vec::new(),
                replies: replies.into_iter().map(|r| r.map(str::to_string)).collect(),
                closed: false,
            }
        }
    }

    impl Wire for FakeWire {
        fn send(&mut self, text: &str) -> Result<(), CdpError> {
            self.sent.push(text.to_string());
            Ok(())
        }

        fn recv(&mut self) -> Result<Option<String>, CdpError> {
            match self.replies.pop_front() {
                Some(reply) => Ok(reply),
                None if self.closed => Err(CdpError::Closed),
                None => Ok(None),
            }
        }
    }

    #[test]
    fn call_matches_ids_and_keeps_events() {
        let wire = FakeWire::new(vec![
            Some(r#"{"method":"Page.loadEventFired","params":{}}"#),
            None,
            Some(r#"{"id":1,"result":{"ok":true}}"#),
            Some(r#"{"id":2,"error":{"code":-32000,"message":"No target"}}"#),
        ]);
        let mut cdp = Cdp::new(wire);
        let result = cdp
            .call(Some("S1"), "Page.enable", json!({}), COMMAND_TIMEOUT)
            .unwrap();
        assert_eq!(result["ok"], Value::from(true));
        assert_eq!(cdp.events_seen(), 1);
        let error = cdp
            .call(None, "Target.getTargets", json!({}), COMMAND_TIMEOUT)
            .unwrap_err();
        assert_eq!(error, CdpError::Remote("No target".into()));

        let sent: Value = serde_json::from_str(&cdp.wire.sent[0]).unwrap();
        assert_eq!(sent["id"], Value::from(1));
        assert_eq!(sent["sessionId"], Value::from("S1"));
        let second: Value = serde_json::from_str(&cdp.wire.sent[1]).unwrap();
        assert_eq!(second["id"], Value::from(2));
        assert_eq!(second.get("sessionId"), None);
    }

    #[test]
    fn call_reports_timeout_and_closed_connections() {
        let mut cdp = Cdp::new(FakeWire::new(vec![None]));
        let error = cdp
            .call(None, "Browser.getVersion", json!({}), Duration::ZERO)
            .unwrap_err();
        assert_eq!(error, CdpError::Timeout);

        let mut wire = FakeWire::new(vec![]);
        wire.closed = true;
        let mut cdp = Cdp::new(wire);
        let error = cdp
            .call(None, "Browser.getVersion", json!({}), COMMAND_TIMEOUT)
            .unwrap_err();
        assert_eq!(error, CdpError::Closed);
    }

    #[test]
    fn evaluate_returns_values_and_rejects_page_exceptions() {
        let wire = FakeWire::new(vec![
            Some(r#"{"id":1,"result":{"result":{"type":"string","value":"https://x/"}}}"#),
            Some(r#"{"id":2,"result":{"result":{},"exceptionDetails":{"text":"boom"}}}"#),
            Some(r#"{"id":3,"result":{"errorText":"net::ERR_NAME_NOT_RESOLVED"}}"#),
        ]);
        let mut cdp = Cdp::new(wire);
        let mut page = Page {
            cdp: &mut cdp,
            session: "S".into(),
        };
        assert_eq!(page.evaluate("location.href").unwrap(), "https://x/");
        assert!(page.evaluate("broken(").is_err());
        let error = page.navigate("https://x/").unwrap_err();
        assert!(matches!(error, CdpError::Protocol(_)));
        assert!(error.message().contains("network"));
    }

    #[test]
    fn insert_text_never_appears_in_an_expression() {
        let wire = FakeWire::new(vec![Some(r#"{"id":1,"result":{}}"#)]);
        let mut cdp = Cdp::new(wire);
        let mut page = Page {
            cdp: &mut cdp,
            session: "S".into(),
        };
        page.insert_text("a-secret").unwrap();
        let sent: Value = serde_json::from_str(&cdp.wire.sent[0]).unwrap();
        assert_eq!(sent["method"], Value::from("Input.insertText"));
        assert_eq!(sent["params"]["text"], Value::from("a-secret"));
    }

    #[test]
    fn page_target_is_picked_out_of_the_target_list() {
        let targets = json!({"targetInfos": [
            {"type": "browser", "targetId": "b1"},
            {"type": "page", "targetId": "p1"},
            {"type": "page", "targetId": "p2"}
        ]});
        assert_eq!(first_page_target(&targets).unwrap(), "p1");
        assert!(first_page_target(&json!({"targetInfos": []})).is_none());
        assert!(first_page_target(&json!({})).is_none());
    }
}
