//! Best-effort, read-only AT-SPI context. No accessibility settings are changed.
//!
//! AT-SPI offsets count Unicode characters, not UTF-8 bytes. Only two bounded
//! ranges outside the selection are requested, with explicit nonnegative ends
//! (never the GetText `-1` all-text sentinel). No accessible names are requested.
//! Ranges exclude the validated selection, with rechecks between reads; AT-SPI
//! cannot make the reads atomic with concurrent focus/selection changes.
//! Incomplete discovery is a failure, not evidence of focus.
//! AT-SPI has no atomic snapshot operation: repeated metadata/context checks
//! detect changes between reads, but cannot eliminate the final insertion race.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use zbus::{DynamicType, OwnedObjectPath, OwnedVariant, Type};

use crate::insertion::SurroundingText;

const TIMEOUT: Duration = Duration::from_millis(250);
/// Insertion reads happen after key release, on the latency-critical path.
/// Missing this deadline only means unadapted (plain) insertion.
const INSERTION_TIMEOUT: Duration = Duration::from_millis(150);
const CONTEXT: i32 = 256;
const MAX_APPS: usize = 32;
const MAX_WINDOWS: usize = 64;
const MAX_NODES: usize = 128;
const MAX_DEPTH: usize = 16;
const MAX_REPLY_BYTES: usize = 64 * 1024;
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const TEXT: &str = "org.a11y.atspi.Text";
const COLLECTION: &str = "org.a11y.atspi.Collection";
const ROOT: &str = "/org/a11y/atspi/accessible/root";

// Stable AT-SPI enum values, not toolkit-specific role/state names. GetState
// returns exactly two uint32 words, with bit N representing state N.
const ACTIVE: u32 = 1;
const DEFUNCT: u32 = 6;
const EDITABLE: u32 = 7;
const FOCUSED: u32 = 12;
const SENSITIVE: u32 = 24;
const SHOWING: u32 = 25;
const STALE: u32 = 27;
const VISIBLE: u32 = 30;
const READ_ONLY: u32 = 43;
const PASSWORD: u32 = 40;
const TERMINAL: u32 = 60;

type Object = (String, OwnedObjectPath); // AT-SPI ObjectReference: (so), not (ss).
type MatchRule = (
    Vec<i32>,
    i32,
    HashMap<String, String>,
    i32,
    Vec<i32>,
    i32,
    Vec<String>,
    i32,
    bool,
);

#[derive(Debug)]
pub struct ReadReport {
    pub context: Option<SurroundingText>,
    pub status: &'static str,
    pub source: &'static str,
}

/// The compositor and accessibility providers share one deadline. Diagnostic
/// strings contain only fixed labels, never document text or accessible names.
pub async fn read_report(session: &zbus::Connection) -> ReadReport {
    read_report_within(session, TIMEOUT).await
}

/// Non-GNOME insertion: the same providers under a tighter deadline.
pub async fn read_insertion_report(session: &zbus::Connection) -> ReadReport {
    read_report_within(session, INSERTION_TIMEOUT).await
}

/// One accessibility-bus connection is reused across reads; a fresh
/// connection's authentication handshake would be paid after every utterance.
/// It is dropped whenever a read fails, so a restarted bus reconnects.
static ACCESSIBILITY_BUS: tokio::sync::Mutex<Option<(String, zbus::Connection)>> =
    tokio::sync::Mutex::const_new(None);

async fn accessibility_bus(address: &str) -> Option<zbus::Connection> {
    let mut cached = ACCESSIBILITY_BUS.lock().await;
    if let Some((cached_address, bus)) = cached.as_ref()
        && cached_address == address
    {
        return Some(bus.clone());
    }
    let bus = zbus::connection::Builder::address(address)
        .build()
        .await
        .ok()?;
    *cached = Some((address.to_owned(), bus.clone()));
    Some(bus)
}

async fn forget_accessibility_bus(address: &str) {
    let mut cached = ACCESSIBILITY_BUS.lock().await;
    if cached
        .as_ref()
        .is_some_and(|(cached_address, _)| cached_address == address)
    {
        *cached = None;
    }
}

async fn read_report_within(session: &zbus::Connection, deadline: Duration) -> ReadReport {
    let mut status = "accessibility-bus-unavailable";
    let mut source = "at-spi";
    let result = tokio::time::timeout(deadline, async {
        // The input-method route can work even when an application's
        // accessibility tree is disabled. Older extensions simply lack it.
        if let Ok(Some(reply)) =
            tokio::time::timeout(Duration::from_millis(130), compositor_context(session)).await
        {
            source = "gnome-input-method";
            match reply {
                Ok(context) => return Some(context),
                Err(reason) => {
                    status = reason;
                    return None;
                }
            }
        }
        source = "at-spi";
        status = "accessibility-bus-unavailable";
        let address: String = call(
            session,
            &(
                "org.a11y.Bus".into(),
                OwnedObjectPath::try_from("/org/a11y/bus").ok()?,
            ),
            "org.a11y.Bus",
            "GetAddress",
            &(),
        )
        .await?;
        if !address.starts_with("unix:") {
            return None;
        }
        let bus = accessibility_bus(&address).await?;
        let context = read_bus(&bus, &mut status).await;
        if context.is_none() {
            forget_accessibility_bus(&address).await;
        }
        context
    })
    .await;
    match result {
        Ok(Some(context)) => {
            let status = if context.before.is_empty() && context.after.is_empty() {
                "empty-field"
            } else {
                "available"
            };
            ReadReport {
                context: Some(context),
                status,
                source,
            }
        }
        Ok(None) => ReadReport {
            context: None,
            status,
            source,
        },
        Err(_) => ReadReport {
            context: None,
            status: "timeout",
            source,
        },
    }
}

/// GNOME insertion must not use an unbound compositor or AT-SPI snapshot.
/// Old loaded extensions still accept plain insertion, but cannot safely adapt
/// punctuation/spacing for a range they do not bind to the eventual key commit.
/// Recognition continues to use read_report and never holds a future target.
pub async fn read_gnome_insertion_report(
    shell: &crate::dbus::ShellProxy<'_>,
) -> (ReadReport, Option<String>) {
    let mut report = ReadReport {
        context: None,
        status: "unbound-extension",
        source: "gnome-input-method",
    };
    let reply = tokio::time::timeout(
        Duration::from_millis(130),
        shell.inner().call_method("GetInsertionContext", &()),
    )
    .await;
    let reply = match reply {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) if crate::inject::unknown_method(&error) => return (report, None),
        Ok(Err(_)) => {
            report.status = "unavailable";
            return (report, None);
        }
        Err(_) => {
            report.status = "timeout";
            return (report, None);
        }
    };
    // Preserve the provider's raw reply limit before allocating text/token strings.
    let body = reply.body();
    if body.len() > MAX_REPLY_BYTES {
        report.status = "invalid-handoff";
        return (report, None);
    }
    let Ok((status, before, after, is_replacement, token)) =
        body.deserialize::<(String, String, String, bool, String)>()
    else {
        report.status = "invalid-handoff";
        return (report, None);
    };
    report.status = match status.as_str() {
        "available" => "available",
        "empty" => "empty-field",
        "protected" => "protected-field",
        "terminal" => "terminal-field",
        "changed" => "changed",
        "timeout" => "timeout",
        "denied" => "denied",
        _ => "unavailable",
    };
    if matches!(status.as_str(), "available" | "empty") {
        if token.is_empty()
            || token.len() > 128
            || before.chars().count() > CONTEXT as usize
            || after.chars().count() > CONTEXT as usize
        {
            report.status = "invalid-handoff";
            return (report, None);
        }
        report.context = Some(SurroundingText {
            before,
            after,
            is_replacement,
        });
        (report, Some(token))
    } else {
        (report, None)
    }
}

async fn compositor_context(
    session: &zbus::Connection,
) -> Option<Result<SurroundingText, &'static str>> {
    let object = (
        "org.localwispr.Shell".into(),
        OwnedObjectPath::try_from("/org/localwispr/Shell").ok()?,
    );
    let (status, before, after, is_replacement): (String, String, String, bool) = call(
        session,
        &object,
        "org.localwispr.Shell1",
        "GetCursorContext",
        &(),
    )
    .await?;
    // Terminal context must not fall through to another provider: shell
    // commands are case-sensitive and are not prose.
    match status.as_str() {
        "protected" => return Some(Err("protected-field")),
        "terminal" => return Some(Err("terminal-field")),
        _ => {}
    }
    if !matches!(status.as_str(), "available" | "empty")
        || before.chars().count() > CONTEXT as usize
        || after.chars().count() > CONTEXT as usize
    {
        return None;
    }
    Some(Ok(SurroundingText {
        before,
        after,
        is_replacement,
    }))
}

#[cfg(test)]
pub async fn read(session: &zbus::Connection) -> Option<SurroundingText> {
    read_report(session).await.context
}

async fn call<R, B>(
    bus: &zbus::Connection,
    object: &Object,
    interface: &str,
    method: &str,
    body: &B,
) -> Option<R>
where
    R: DeserializeOwned + Type,
    B: Serialize + DynamicType,
{
    let reply = bus
        .call_method(
            Some(object.0.as_str()),
            object.1.as_str(),
            Some(interface),
            method,
            body,
        )
        .await
        .ok()?;
    let body = reply.body();
    let limit = if interface == TEXT && method == "GetText" {
        CONTEXT as usize * 4 + 8 // UTF-8 maximum plus D-Bus string framing.
    } else {
        MAX_REPLY_BYTES
    };
    // Check before deserializing vectors/maps/strings from an oversized reply.
    if body.len() > limit {
        return None;
    }
    body.deserialize().ok()
}

async fn property(bus: &zbus::Connection, object: &Object, name: &str) -> Option<i32> {
    // Explicit Properties.Get avoids proxy caches and implicit GetAll calls.
    let value: OwnedVariant = call(
        bus,
        object,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(TEXT, name),
    )
    .await?;
    i32::try_from(value).ok()
}

async fn state(bus: &zbus::Connection, object: &Object) -> Option<[u32; 2]> {
    let words: Vec<u32> = call(bus, object, ACCESSIBLE, "GetState", &()).await?;
    words.try_into().ok()
}

fn has(state: &[u32; 2], bit: u32) -> bool {
    state[(bit / 32) as usize] & (1 << (bit % 32)) != 0
}

fn alive(state: &[u32; 2]) -> bool {
    !has(state, DEFUNCT) && !has(state, STALE)
}

fn editable(state: &[u32; 2]) -> bool {
    alive(state)
        // GTK4 exports SENSITIVE, but does not export ENABLED for its fields.
        && [FOCUSED, EDITABLE, SENSITIVE, SHOWING, VISIBLE]
            .iter()
            .all(|bit| has(state, *bit))
        && !has(state, READ_ONLY)
}

fn text_role(role: u32) -> bool {
    // Combo box, text, paragraph, entry, section, text document, web document.
    // Browsers expose rich-text editors as editable containers, not just entries.
    matches!(role, 11 | 61 | 73 | 79 | 85 | 94 | 95)
}

async fn role(bus: &zbus::Connection, object: &Object) -> Option<u32> {
    call(bus, object, ACCESSIBLE, "GetRole", &()).await
}

async fn children(bus: &zbus::Connection, object: &Object, limit: usize) -> Option<Vec<Object>> {
    let children: Vec<Object> = call(bus, object, ACCESSIBLE, "GetChildren", &()).await?;
    if children.len() > limit {
        return None;
    }
    let mut seen = HashSet::new();
    for child in &children {
        // Pin references to unique owners. A well-known name could change owner
        // between validation and text reads. Reject null and duplicate references.
        zbus::names::UniqueName::try_from(child.0.as_str()).ok()?;
        if child.1.as_str() == "/org/a11y/atspi/null" || !seen.insert(child) {
            return None;
        }
    }
    Some(children)
}

async fn active_window(bus: &zbus::Connection) -> Option<Object> {
    let desktop = (
        "org.a11y.atspi.Registry".into(),
        OwnedObjectPath::try_from(ROOT).ok()?,
    );
    let mut active = None;
    let mut windows_left = MAX_WINDOWS;
    for app in children(bus, &desktop, MAX_APPS).await? {
        // Only inspect immediate application children to discover active windows.
        // Never descend into inactive application/window subtrees.
        for window in children(bus, &app, windows_left).await? {
            windows_left = windows_left.checked_sub(1)?;
            let states = state(bus, &window).await?;
            if has(&states, ACTIVE) {
                if !alive(&states)
                    || !matches!(role(bus, &window).await?, 2 | 16 | 19 | 23 | 69)
                    || active.is_some()
                {
                    return None;
                }
                active = Some(window);
            }
        }
    }
    active
}

struct Focus {
    object: Object,
    // Includes the active window and text object; revalidate the complete route.
    route: Vec<Object>,
}

async fn find_focus(bus: &zbus::Connection, window: Object) -> Option<Focus> {
    let interfaces: Vec<String> = call(bus, &window, ACCESSIBLE, "GetInterfaces", &()).await?;
    if interfaces.iter().any(|interface| interface == COLLECTION)
        && let Some(matches) = collection_matches(bus, &window).await
        && !matches.is_empty()
    {
        // A definite but ambiguous/invalid result must not be overridden.
        return collection_focus(bus, window, matches).await;
    }
    // Some toolkits advertise Collection without implementing focused-text
    // matching. An empty/error reply is not proof there is no editable field;
    // try the same bounded, fully validated traversal used by other toolkits.
    let mut pending = vec![(window.clone(), vec![window])];
    let mut visited = HashSet::new();
    let mut focus = None;
    while let Some((object, route)) = pending.pop() {
        if visited.len() >= MAX_NODES || route.len() > MAX_DEPTH || !visited.insert(object.clone())
        {
            return None;
        }
        let states = state(bus, &object).await?;
        if !alive(&states) {
            return None;
        }
        let role = role(bus, &object).await?;
        if has(&states, FOCUSED) {
            if focus.is_some() || !editable(&states) || !text_role(role) {
                return None;
            }
            focus = Some(Focus {
                object: object.clone(),
                route: route.clone(),
            });
        }
        if matches!(role, PASSWORD | TERMINAL) {
            continue;
        }
        let next = children(bus, &object, MAX_NODES - visited.len()).await?;
        // Account for queued siblings as well as the current node's children.
        if visited.len() + pending.len() + next.len() > MAX_NODES {
            return None;
        }
        for child in next {
            let mut child_route = route.clone();
            child_route.push(child.clone());
            pending.push((child, child_route));
        }
    }
    focus
}

async fn collection_matches(bus: &zbus::Connection, window: &Object) -> Option<Vec<Object>> {
    // Collection performs the search in the application, avoiding hundreds of
    // cross-process requests over unrelated document nodes. Ask for two matches
    // so ambiguous focus cannot silently become the first result.
    let rule: MatchRule = (
        vec![(1 << FOCUSED) | (1 << EDITABLE), 0],
        1,
        HashMap::new(),
        1,
        vec![0; 4],
        1,
        vec!["Text".into()],
        1,
        false,
    );
    call(
        bus,
        window,
        COLLECTION,
        "GetMatches",
        &(rule, 1u32, 2i32, true),
    )
    .await
}

async fn collection_focus(
    bus: &zbus::Connection,
    window: Object,
    matches: Vec<Object>,
) -> Option<Focus> {
    if matches.len() != 1 {
        return None;
    }
    let object = matches.into_iter().next()?;
    let mut current = object.clone();
    let mut route = Vec::new();
    let mut visited = HashSet::new();
    loop {
        zbus::names::UniqueName::try_from(current.0.as_str()).ok()?;
        if current.1.as_str() == "/org/a11y/atspi/null"
            || route.len() >= MAX_DEPTH
            || !visited.insert(current.clone())
        {
            return None;
        }
        route.push(current.clone());
        if current == window {
            break;
        }
        let parent: OwnedVariant = call(
            bus,
            &current,
            "org.freedesktop.DBus.Properties",
            "Get",
            &(ACCESSIBLE, "Parent"),
        )
        .await?;
        current = Object::try_from(parent).ok()?;
    }
    route.reverse();
    Some(Focus { object, route })
}

fn protected(attributes: &HashMap<String, String>) -> bool {
    attributes.iter().any(|(key, value)| {
        let key = key.to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        // Include toolkit/browser hints as well as the standard password role.
        (matches!(
            key.as_str(),
            "password" | "protected" | "is-password" | "is-protected" | "hidden" | "invisible"
        ) && matches!(
            value.as_str(),
            "true" | "1" | "yes" | "password" | "protected" | "secret"
        )) || (matches!(key.as_str(), "text-input-type" | "input-type" | "type")
            && value == "password")
    })
}

async fn validate_focus(bus: &zbus::Connection, focus: &Focus) -> Option<()> {
    for (index, object) in focus.route.iter().enumerate() {
        let states = state(bus, object).await?;
        let role = role(bus, object).await?;
        let attributes: HashMap<String, String> =
            call(bus, object, ACCESSIBLE, "GetAttributes", &()).await?;
        if !alive(&states)
            || matches!(role, PASSWORD | TERMINAL)
            || protected(&attributes)
            || (index == 0 && (!has(&states, ACTIVE) || !matches!(role, 2 | 16 | 19 | 23 | 69)))
        {
            return None;
        }
        if object == &focus.object && (!editable(&states) || !text_role(role)) {
            return None;
        }
    }
    let interfaces: Vec<String> =
        call(bus, &focus.object, ACCESSIBLE, "GetInterfaces", &()).await?;
    interfaces
        .iter()
        .any(|interface| interface == TEXT)
        .then_some(())
}

#[derive(Debug, PartialEq, Eq)]
struct Position {
    count: i32,
    caret: i32,
    selection: Option<(i32, i32)>,
}

impl Position {
    fn edges(&self) -> (i32, i32) {
        self.selection.unwrap_or((self.caret, self.caret))
    }
}

async fn position(bus: &zbus::Connection, object: &Object) -> Option<Position> {
    let count = property(bus, object, "CharacterCount").await?;
    let caret = property(bus, object, "CaretOffset").await?;
    if count < 0 || !(0..=count).contains(&caret) {
        return None;
    }
    let selections: i32 = call(bus, object, TEXT, "GetNSelections", &()).await?;
    let selection = match selections {
        0 => None,
        1 => {
            let (start, end): (i32, i32) =
                call(bus, object, TEXT, "GetSelection", &(0i32,)).await?;
            if start < 0 || end < start || end > count || (caret != start && caret != end) {
                return None;
            }
            Some((start, end))
        }
        _ => return None,
    };
    Some(Position {
        count,
        caret,
        selection,
    })
}

async fn text_range(
    bus: &zbus::Connection,
    object: &Object,
    start: i32,
    end: i32,
) -> Option<String> {
    if start == end {
        return Some(String::new());
    }
    if start < 0 || end < start || end - start > CONTEXT {
        return None;
    }
    let text: String = call(bus, object, TEXT, "GetText", &(start, end)).await?;
    // Detect byte/UTF-16 offsets, truncation and malformed implementations. Never
    // salvage a partial reply, since its relationship to the caret is uncertain.
    (text.chars().count() == (end - start) as usize).then_some(text)
}

async fn read_bus(bus: &zbus::Connection, status: &mut &'static str) -> Option<SurroundingText> {
    *status = "no-active-window";
    let window = active_window(bus).await?;
    *status = "no-focused-editor";
    let focus = find_focus(bus, window).await?;
    *status = "unsupported-or-protected-editor";
    validate_focus(bus, &focus).await?;
    *status = "invalid-caret-or-selection";
    let pos = position(bus, &focus.object).await?;
    *status = "context-changed-or-unreadable";
    let object = &focus.object;
    let (start, end) = pos.edges();
    let before = text_range(bus, object, start.saturating_sub(CONTEXT).max(0), start).await?;
    // A selection/focus change after the first range must not cause the second
    // request to read newly selected or protected contents at stale offsets.
    validate_focus(bus, &focus).await?;
    if position(bus, object).await? != pos {
        return None;
    }
    let after = text_range(bus, object, end, end.saturating_add(CONTEXT).min(pos.count)).await?;
    // Final check: the context still describes the validated caret. A
    // same-length edit between the two short reads is not detected; the
    // context only steers capitalization/spacing, and every check above runs
    // after the user has stopped typing (key release).
    if position(bus, object).await? != pos {
        return None;
    }
    validate_focus(bus, &focus).await?;
    Some(SurroundingText {
        before,
        after,
        is_replacement: end > start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    const APP: &str = "/test/app";
    const WINDOW: &str = "/test/window";
    const FIELD: &str = "/test/field";
    const OTHER_WINDOW: &str = "/test/inactive";
    const OTHER_FIELD: &str = "/test/other";

    #[derive(Clone)]
    struct Node {
        role: u32,
        states: Vec<u32>,
        children: Vec<&'static str>,
        attributes: HashMap<String, String>,
        text_interface: bool,
    }

    fn bits(values: &[u32]) -> Vec<u32> {
        let mut words = vec![0; 2];
        for bit in values {
            words[(bit / 32) as usize] |= 1 << (bit % 32);
        }
        words
    }

    fn node(role: u32, states: &[u32], children: Vec<&'static str>) -> Node {
        Node {
            role,
            states: bits(states),
            children,
            attributes: HashMap::new(),
            text_interface: true,
        }
    }

    #[derive(Clone, Copy, Default)]
    enum Change {
        #[default]
        None,
        Caret,
        Selection,
        Count,
        Text,
        Focus,
        Active,
        Protection,
        SlowText,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Call {
        Children(&'static str),
        Matches,
        Property(&'static str),
        Selection(i32),
        Text(i32, i32),
    }

    struct Model {
        nodes: HashMap<&'static str, Node>,
        owner: String,
        text: String,
        count_override: Option<i32>,
        caret: i32,
        selection: Option<(i32, i32)>,
        selections_override: Option<i32>,
        change: Change,
        reads: usize,
        calls: Vec<Call>,
        fail_text: bool,
        malformed_text: bool,
        slow_address: bool,
        collection_matches: Option<Vec<&'static str>>,
        collection_rule: Option<(MatchRule, u32, i32, bool)>,
        metadata_delay: Duration,
        shell_context: Option<(String, String, String, bool)>,
        shell_insertion_context: Option<(String, String, String, bool, String)>,
        slow_shell_insertion: bool,
    }

    impl Default for Model {
        fn default() -> Self {
            Self {
                nodes: HashMap::from([
                    (ROOT, node(14, &[], vec![APP])),
                    (APP, node(75, &[], vec![WINDOW, OTHER_WINDOW])),
                    (WINDOW, node(23, &[ACTIVE], vec![FIELD])),
                    (
                        FIELD,
                        node(
                            79,
                            &[FOCUSED, EDITABLE, SENSITIVE, SHOWING, VISIBLE],
                            vec![],
                        ),
                    ),
                    // A stale focused field in an inactive window must never be
                    // visited, much less read. Only its window state is queried.
                    (OTHER_WINDOW, node(23, &[], vec![OTHER_FIELD])),
                    (OTHER_FIELD, node(79, &[FOCUSED, EDITABLE], vec![])),
                ]),
                owner: String::new(),
                text: "abcDEFghi".into(),
                count_override: None,
                caret: 3,
                selection: None,
                selections_override: None,
                change: Change::None,
                reads: 0,
                calls: vec![],
                fail_text: false,
                malformed_text: false,
                slow_address: false,
                collection_matches: None,
                collection_rule: None,
                metadata_delay: Duration::ZERO,
                shell_context: None,
                shell_insertion_context: None,
                slow_shell_insertion: false,
            }
        }
    }

    type Shared = Arc<Mutex<Model>>;

    struct MockBus {
        address: String,
        model: Shared,
    }

    async fn delay(duration: Duration) {
        // A timed-out server handler can outlive the test's Tokio runtime.
        // Use an executor-neutral signal so teardown cannot panic on timers.
        let (send, receive) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            std::thread::sleep(duration);
            let _ = send.send(());
        });
        let _ = receive.await;
    }

    #[zbus::interface(name = "org.a11y.Bus")]
    impl MockBus {
        async fn get_address(&self) -> String {
            let slow = self.model.lock().unwrap().slow_address;
            if slow {
                delay(Duration::from_secs(1)).await;
            }
            self.address.clone()
        }
    }

    struct MockAccessible {
        path: &'static str,
        model: Shared,
    }

    #[zbus::interface(name = "org.a11y.atspi.Accessible")]
    impl MockAccessible {
        fn get_children(&self) -> Vec<Object> {
            let mut model = self.model.lock().unwrap();
            model.calls.push(Call::Children(self.path));
            model.nodes[self.path]
                .children
                .iter()
                .map(|path| {
                    (
                        model.owner.clone(),
                        OwnedObjectPath::try_from(*path).unwrap(),
                    )
                })
                .collect()
        }

        async fn get_state(&self) -> Vec<u32> {
            let duration = self.model.lock().unwrap().metadata_delay;
            if !duration.is_zero() {
                delay(duration).await;
            }
            self.model.lock().unwrap().nodes[self.path].states.clone()
        }

        fn get_role(&self) -> u32 {
            self.model.lock().unwrap().nodes[self.path].role
        }

        fn get_attributes(&self) -> HashMap<String, String> {
            self.model.lock().unwrap().nodes[self.path]
                .attributes
                .clone()
        }

        fn get_interfaces(&self) -> Vec<String> {
            let model = self.model.lock().unwrap();
            let mut interfaces = if model.nodes[self.path].text_interface {
                vec![ACCESSIBLE.into(), TEXT.into()]
            } else {
                vec![ACCESSIBLE.into()]
            };
            if self.path == WINDOW && model.collection_matches.is_some() {
                interfaces.push(COLLECTION.into());
            }
            interfaces
        }

        #[zbus(property)]
        fn parent(&self) -> Object {
            let model = self.model.lock().unwrap();
            let parent = model
                .nodes
                .iter()
                .find_map(|(path, node)| node.children.contains(&self.path).then_some(*path))
                .unwrap_or("/org/a11y/atspi/null");
            (
                model.owner.clone(),
                OwnedObjectPath::try_from(parent).unwrap(),
            )
        }
    }

    struct MockCollection(Shared);

    struct MockShell(Shared);

    #[zbus::interface(name = "org.localwispr.Shell1")]
    impl MockShell {
        #[zbus(out_args("status", "before", "after", "is_replacement"))]
        fn get_cursor_context(&self) -> (String, String, String, bool) {
            self.0.lock().unwrap().shell_context.clone().unwrap()
        }

        #[zbus(out_args("status", "before", "after", "is_replacement", "context_token"))]
        async fn get_insertion_context(
            &self,
        ) -> zbus::fdo::Result<(String, String, String, bool, String)> {
            let (slow, reply) = {
                let model = self.0.lock().unwrap();
                (
                    model.slow_shell_insertion,
                    model.shell_insertion_context.clone(),
                )
            };
            if slow {
                delay(Duration::from_millis(250)).await;
            }
            reply.ok_or_else(|| zbus::fdo::Error::UnknownMethod("GetInsertionContext".into()))
        }
    }

    #[zbus::interface(name = "org.a11y.atspi.Collection")]
    impl MockCollection {
        fn get_matches(
            &self,
            rule: MatchRule,
            sortby: u32,
            count: i32,
            traverse: bool,
        ) -> Vec<Object> {
            let mut model = self.0.lock().unwrap();
            model.calls.push(Call::Matches);
            model.collection_rule = Some((rule, sortby, count, traverse));
            model
                .collection_matches
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|path| {
                    (
                        model.owner.clone(),
                        OwnedObjectPath::try_from(*path).unwrap(),
                    )
                })
                .collect()
        }
    }

    struct MockText(Shared);

    #[zbus::interface(name = "org.a11y.atspi.Text")]
    impl MockText {
        #[zbus(property)]
        fn character_count(&self) -> i32 {
            let mut model = self.0.lock().unwrap();
            model.calls.push(Call::Property("CharacterCount"));
            model
                .count_override
                .unwrap_or(model.text.chars().count() as i32)
        }

        #[zbus(property)]
        fn caret_offset(&self) -> i32 {
            let mut model = self.0.lock().unwrap();
            model.calls.push(Call::Property("CaretOffset"));
            model.caret
        }

        fn get_n_selections(&self) -> i32 {
            let model = self.0.lock().unwrap();
            model
                .selections_override
                .unwrap_or(i32::from(model.selection.is_some()))
        }

        // Two OUT arguments have signature ii, not a single struct (ii).
        #[zbus(out_args("start_offset", "end_offset"))]
        fn get_selection(&self, selection_num: i32) -> (i32, i32) {
            let mut model = self.0.lock().unwrap();
            model.calls.push(Call::Selection(selection_num));
            model.selection.unwrap_or((-1, -1))
        }

        async fn get_text(&self, start_offset: i32, end_offset: i32) -> zbus::fdo::Result<String> {
            let slow = matches!(self.0.lock().unwrap().change, Change::SlowText);
            if slow {
                delay(Duration::from_secs(1)).await;
            }
            let mut model = self.0.lock().unwrap();
            model.calls.push(Call::Text(start_offset, end_offset));
            if model.fail_text {
                return Err(zbus::fdo::Error::AccessDenied("synthetic denial".into()));
            }
            assert!(start_offset >= 0 && end_offset >= start_offset);
            assert!(end_offset - start_offset <= CONTEXT);
            if let Some((start, end)) = model.selection {
                assert!(
                    end_offset <= start || start_offset >= end,
                    "selection must not be fetched"
                );
            }
            let result = if model.malformed_text {
                String::new()
            } else {
                model
                    .text
                    .chars()
                    .skip(start_offset as usize)
                    .take((end_offset - start_offset) as usize)
                    .collect()
            };
            model.reads += 1;
            if model.reads == 1 {
                match model.change {
                    Change::Caret => model.caret += 1,
                    Change::Selection => model.selection = Some((3, 9)),
                    Change::Count => model.text.push('!'),
                    Change::Text => model.text.replace_range(0..1, "z"),
                    Change::Focus => model.nodes.get_mut(FIELD).unwrap().states = bits(&[EDITABLE]),
                    Change::Active => model.nodes.get_mut(WINDOW).unwrap().states = bits(&[]),
                    Change::Protection => {
                        model
                            .nodes
                            .get_mut(FIELD)
                            .unwrap()
                            .attributes
                            .insert("protected".into(), "true".into());
                    }
                    Change::None | Change::SlowText => {}
                }
            }
            Ok(result)
        }
    }

    // This daemon is a private test transport, never the user's session or
    // accessibility bus. No real application is launched or inspected.
    struct PrivateBus(Child);

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct Fixture {
        model: Shared,
        session: zbus::Connection,
        _server: zbus::Connection,
        _daemon: PrivateBus,
    }

    impl Fixture {
        async fn new(model: Model) -> Self {
            let child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("mock D-Bus fixtures require dbus-daemon");
            let mut daemon = PrivateBus(child);
            let mut address = String::new();
            BufReader::new(daemon.0.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            let address = address.trim().to_owned();
            let model = Arc::new(Mutex::new(model));
            let server = zbus::connection::Builder::address(address.as_str())
                .serve_at(
                    "/org/a11y/bus",
                    MockBus {
                        address: address.clone(),
                        model: model.clone(),
                    },
                )
                .build()
                .await
                .unwrap();
            model.lock().unwrap().owner = server.unique_name().unwrap().to_string();
            let paths = model
                .lock()
                .unwrap()
                .nodes
                .keys()
                .copied()
                .collect::<Vec<_>>();
            for path in paths {
                server
                    .object_server()
                    .at(
                        path,
                        MockAccessible {
                            path,
                            model: model.clone(),
                        },
                    )
                    .await
                    .unwrap();
            }
            server
                .object_server()
                .at(FIELD, MockText(model.clone()))
                .await
                .unwrap();
            server
                .object_server()
                .at(WINDOW, MockCollection(model.clone()))
                .await
                .unwrap();
            let has_shell = {
                let model = model.lock().unwrap();
                model.shell_context.is_some() || model.shell_insertion_context.is_some()
            };
            if has_shell {
                server
                    .object_server()
                    .at("/org/localwispr/Shell", MockShell(model.clone()))
                    .await
                    .unwrap();
                server.request_name("org.localwispr.Shell").await.unwrap();
            }
            server.request_name("org.a11y.Bus").await.unwrap();
            server
                .request_name("org.a11y.atspi.Registry")
                .await
                .unwrap();
            let session = zbus::connection::Builder::address(address.as_str())
                .build()
                .await
                .unwrap();
            Self {
                model,
                session,
                _server: server,
                _daemon: daemon,
            }
        }

        fn no_text_read(&self) {
            assert!(
                !self
                    .model
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .any(|call| matches!(call, Call::Text(..)))
            );
        }
    }

    #[tokio::test]
    async fn real_protocol_caret_and_inactive_window_fallback() {
        let fixture = Fixture::new(Model::default()).await;
        let result = read(&fixture.session).await.unwrap();
        assert_eq!(result.before, "abc");
        assert_eq!(result.after, "DEFghi");
        assert!(!result.is_replacement);
        let model = fixture.model.lock().unwrap();
        assert!(!model.calls.contains(&Call::Children(OTHER_WINDOW)));
        assert!(!model.calls.contains(&Call::Children(OTHER_FIELD)));
        assert!(
            model
                .calls
                .iter()
                .filter(|call| **call == Call::Property("CaretOffset"))
                .count()
                >= 3
        );
        assert_eq!(
            model
                .calls
                .iter()
                .filter(|call| matches!(call, Call::Text(..)))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn selection_uses_two_out_arguments_and_never_reads_selected_text() {
        for caret in [3, 6] {
            let fixture = Fixture::new(Model {
                caret,
                selection: Some((3, 6)),
                ..Model::default()
            })
            .await;
            let result = read(&fixture.session).await.unwrap();
            assert_eq!(result.before, "abc");
            assert_eq!(result.after, "ghi");
            assert!(result.is_replacement);
            let model = fixture.model.lock().unwrap();
            assert!(model.calls.contains(&Call::Selection(0)));
            assert!(
                model
                    .calls
                    .iter()
                    .filter(|call| matches!(call, Call::Text(..)))
                    .all(|call| matches!(call, Call::Text(0, 3) | Call::Text(6, 9)))
            );
        }
    }

    #[tokio::test]
    async fn unicode_ranges_are_bounded_in_characters() {
        let fixture = Fixture::new(Model {
            text: "🦀é界".repeat(300),
            caret: 450,
            ..Model::default()
        })
        .await;
        let result = read(&fixture.session).await.unwrap();
        assert_eq!(result.before.chars().count(), 256);
        assert_eq!(result.after.chars().count(), 256);
        let model = fixture.model.lock().unwrap();
        assert!(model.calls.contains(&Call::Text(194, 450)));
        assert!(model.calls.contains(&Call::Text(450, 706)));
    }

    #[tokio::test]
    async fn empty_context_and_document_boundaries_need_no_empty_get_text() {
        for (text, caret, expected_before, expected_after, reads) in [
            ("", 0, "", "", 0),
            ("abc", 0, "", "abc", 1),
            ("abc", 3, "abc", "", 1),
        ] {
            let fixture = Fixture::new(Model {
                text: text.into(),
                caret,
                ..Model::default()
            })
            .await;
            let result = read(&fixture.session).await.unwrap();
            assert_eq!(result.before, expected_before);
            assert_eq!(result.after, expected_after);
            assert_eq!(fixture.model.lock().unwrap().reads, reads);
        }
    }

    #[tokio::test]
    async fn invalid_caret_counts_and_selections_fail_before_text() {
        for (caret, count, selection, n) in [
            (-1, None, None, None),
            (10, None, None, None),
            (3, Some(-1), None, None),
            (3, None, None, Some(-1)),
            (3, None, None, Some(2)),
            (3, None, Some((-1, 6)), None),
            (3, None, Some((6, 3)), None),
            (3, None, Some((3, 10)), None),
            (4, None, Some((3, 6)), None),
        ] {
            let fixture = Fixture::new(Model {
                caret,
                count_override: count,
                selection,
                selections_override: n,
                ..Model::default()
            })
            .await;
            assert!(read(&fixture.session).await.is_none());
            fixture.no_text_read();
        }
    }

    #[tokio::test]
    async fn protected_terminal_unsupported_and_readonly_fail_before_text() {
        for case in 0..9 {
            let mut model = Model::default();
            let field = model.nodes.get_mut(FIELD).unwrap();
            match case {
                0 => field.role = PASSWORD,
                1 => field.role = TERMINAL,
                2 => {
                    field
                        .attributes
                        .insert("text-input-type".into(), "password".into());
                }
                3 => {
                    field.attributes.insert("protected".into(), "true".into());
                }
                4 => field.states = bits(&[FOCUSED]),
                5 => field.states[1] |= 1 << (READ_ONLY - 32),
                6 => field.text_interface = false,
                7 => field.role = 43, // button, even if it claims editable text
                8 => model.nodes.get_mut(WINDOW).unwrap().role = TERMINAL,
                _ => unreachable!(),
            }
            let fixture = Fixture::new(model).await;
            assert!(read(&fixture.session).await.is_none(), "case {case}");
            fixture.no_text_read();
        }
    }

    #[tokio::test]
    async fn missing_or_ambiguous_focus_and_active_windows_fail_before_text() {
        for case in 0..5 {
            let mut model = Model::default();
            match case {
                0 => model.nodes.get_mut(WINDOW).unwrap().states = bits(&[]),
                1 => model.nodes.get_mut(OTHER_WINDOW).unwrap().states = bits(&[ACTIVE]),
                2 => model.nodes.get_mut(FIELD).unwrap().states = bits(&[EDITABLE]),
                3 => {
                    let second = model.nodes[FIELD].clone();
                    model.nodes.insert(OTHER_FIELD, second);
                    model
                        .nodes
                        .get_mut(WINDOW)
                        .unwrap()
                        .children
                        .push(OTHER_FIELD);
                }
                4 => model.nodes.get_mut(FIELD).unwrap().states = vec![0], // malformed au
                _ => unreachable!(),
            }
            let fixture = Fixture::new(model).await;
            assert!(read(&fixture.session).await.is_none(), "case {case}");
            fixture.no_text_read();
        }
    }

    #[tokio::test]
    async fn changed_position_context_focus_and_protection_are_rejected() {
        for change in [
            Change::Caret,
            Change::Selection,
            Change::Count,
            Change::Focus,
            Change::Active,
            Change::Protection,
        ] {
            let fixture = Fixture::new(Model {
                change,
                ..Model::default()
            })
            .await;
            assert!(read(&fixture.session).await.is_none());
            if matches!(
                change,
                Change::Selection | Change::Focus | Change::Active | Change::Protection
            ) {
                assert_eq!(fixture.model.lock().unwrap().reads, 1);
            }
        }
    }

    #[tokio::test]
    async fn same_length_edit_between_reads_is_an_accepted_race() {
        // One pass keeps post-release latency low. A same-length edit between
        // the two short reads is not detected: context only steers spacing and
        // case, and the selection is still never read.
        let fixture = Fixture::new(Model {
            change: Change::Text,
            ..Model::default()
        })
        .await;
        let result = read(&fixture.session).await.unwrap();
        assert_eq!(result.before, "abc");
        assert_eq!(fixture.model.lock().unwrap().reads, 2);
    }

    #[tokio::test]
    async fn inaccessible_and_malformed_text_return_none() {
        for malformed in [false, true] {
            let fixture = Fixture::new(Model {
                fail_text: !malformed,
                malformed_text: malformed,
                ..Model::default()
            })
            .await;
            assert!(read(&fixture.session).await.is_none());
        }
    }

    #[tokio::test]
    async fn traversal_limits_duplicates_and_cycles_return_none() {
        for case in 0..4 {
            let mut model = Model::default();
            match case {
                0 => model.nodes.get_mut(ROOT).unwrap().children = vec![APP; MAX_APPS + 1],
                1 => model.nodes.get_mut(APP).unwrap().children = vec![WINDOW; MAX_WINDOWS + 1],
                2 => model.nodes.get_mut(WINDOW).unwrap().children = vec![FIELD; MAX_NODES + 1],
                3 => model.nodes.get_mut(FIELD).unwrap().children.push(WINDOW),
                _ => unreachable!(),
            }
            let fixture = Fixture::new(model).await;
            assert!(read(&fixture.session).await.is_none());
            fixture.no_text_read();
        }
    }

    #[tokio::test]
    async fn deadline_covers_bus_discovery_and_text_calls() {
        for slow_address in [true, false] {
            let fixture = Fixture::new(Model {
                slow_address,
                change: Change::SlowText,
                ..Model::default()
            })
            .await;
            let start = Instant::now();
            assert!(read(&fixture.session).await.is_none());
            assert!(
                start.elapsed() >= TIMEOUT - Duration::from_millis(25),
                "a timeout must not be an early handler failure"
            );
            assert!(start.elapsed() < Duration::from_millis(600));
            fixture.no_text_read();
        }
    }

    #[tokio::test]
    async fn gtk4_and_browser_editable_roles_do_not_require_enabled_state() {
        for role in [11, 61, 73, 79, 85, 94, 95] {
            let mut model = Model::default();
            let field = model.nodes.get_mut(FIELD).unwrap();
            field.role = role;
            assert!(!has(&field.states.clone().try_into().unwrap(), 8));
            let fixture = Fixture::new(model).await;
            assert!(
                read(&fixture.session).await.is_some(),
                "editable role {role}"
            );
        }
    }

    #[tokio::test]
    async fn protection_is_scoped_to_positive_protection_attributes() {
        for (key, value, blocked) in [
            ("protected", "false", false),
            ("protected", "true", true),
            ("class", "protected", false),
            ("id", "password", false),
            ("placeholder", "secret", false),
            ("text-input-type", "password", true),
            ("password", "0", false),
            ("is-password", "yes", true),
        ] {
            let mut model = Model::default();
            model
                .nodes
                .get_mut(WINDOW)
                .unwrap()
                .attributes
                .insert(key.into(), value.into());
            let fixture = Fixture::new(model).await;
            assert_eq!(
                read(&fixture.session).await.is_none(),
                blocked,
                "{key}={value}"
            );
            if blocked {
                fixture.no_text_read();
            }
        }
    }

    #[tokio::test]
    async fn collection_finds_focus_in_a_large_window_without_walking_unrelated_nodes() {
        let mut model = Model {
            collection_matches: Some(vec![FIELD]),
            metadata_delay: Duration::from_millis(1),
            ..Model::default()
        };
        for i in 0..150 {
            let path: &'static str = Box::leak(format!("/test/label{i}").into_boxed_str());
            model
                .nodes
                .insert(path, node(29, &[VISIBLE, SHOWING], vec![]));
            model.nodes.get_mut(WINDOW).unwrap().children.push(path);
        }
        let fixture = Fixture::new(model).await;
        assert!(read(&fixture.session).await.is_some());
        let model = fixture.model.lock().unwrap();
        assert!(model.calls.contains(&Call::Matches));
        assert!(!model.calls.contains(&Call::Children(WINDOW)));
        let (rule, order, count, traverse) = model.collection_rule.as_ref().unwrap();
        assert_eq!(rule.0, [(1 << FOCUSED) | (1 << EDITABLE), 0]);
        assert_eq!(
            (rule.1, rule.3, rule.5, rule.7, rule.8),
            (1, 1, 1, 1, false)
        );
        assert_eq!(rule.6, ["Text"]);
        assert_eq!((*order, *count, *traverse), (1, 2, true));
    }

    #[tokio::test]
    async fn empty_collection_result_uses_validated_tree_fallback() {
        let fixture = Fixture::new(Model {
            collection_matches: Some(vec![]),
            ..Model::default()
        })
        .await;
        assert!(read(&fixture.session).await.is_some());
        assert!(fixture.model.lock().unwrap().collection_rule.is_some());
    }

    #[tokio::test]
    async fn collection_rejects_ambiguous_unrelated_and_protected_results() {
        for (matches, protected) in [
            (vec![FIELD, OTHER_FIELD], false),
            (vec![OTHER_FIELD], false),
            (vec![FIELD], true),
        ] {
            let mut model = Model {
                collection_matches: Some(matches),
                ..Model::default()
            };
            if protected {
                model.nodes.get_mut(FIELD).unwrap().role = PASSWORD;
            }
            let fixture = Fixture::new(model).await;
            assert!(read(&fixture.session).await.is_none());
            fixture.no_text_read();
        }
    }

    #[tokio::test]
    async fn compositor_context_works_without_an_accessibility_editor() {
        let mut model = Model {
            shell_context: Some(("available".into(), "Use ".into(), " now".into(), true)),
            ..Model::default()
        };
        model.nodes.get_mut(WINDOW).unwrap().states = bits(&[]);
        let fixture = Fixture::new(model).await;
        let report = read_report(&fixture.session).await;
        assert_eq!(
            (report.status, report.source),
            ("available", "gnome-input-method")
        );
        let context = report.context.unwrap();
        assert_eq!(context.before, "Use ");
        assert_eq!(context.after, " now");
        assert!(context.is_replacement);
        fixture.no_text_read();
        assert!(fixture.model.lock().unwrap().calls.is_empty());
    }

    #[tokio::test]
    async fn unavailable_compositor_uses_accessibility_but_protected_fields_do_not() {
        for status in ["unavailable", "changed", "timeout", "denied", "protected"] {
            let fixture = Fixture::new(Model {
                shell_context: Some((status.into(), "".into(), "".into(), false)),
                ..Model::default()
            })
            .await;
            let report = read_report(&fixture.session).await;
            if status == "protected" {
                assert!(report.context.is_none());
                assert_eq!(
                    (report.status, report.source),
                    ("protected-field", "gnome-input-method")
                );
                fixture.no_text_read();
            } else {
                assert!(report.context.is_some());
                assert_eq!((report.status, report.source), ("available", "at-spi"));
            }
        }
    }

    #[tokio::test]
    async fn compositor_terminal_context_never_falls_through_or_changes_command_case() {
        let fixture = Fixture::new(Model {
            shell_context: Some(("terminal".into(), "".into(), "".into(), false)),
            ..Model::default()
        })
        .await;
        let report = read_report(&fixture.session).await;
        assert_eq!(report.status, "terminal-field");
        assert!(report.context.is_none());
        assert_eq!(
            crate::insertion::format("echo hello", report.context.as_ref(), true),
            "echo hello "
        );
        fixture.no_text_read();
        assert!(fixture.model.lock().unwrap().calls.is_empty());
    }

    #[tokio::test]
    async fn diagnostics_distinguish_missing_editor_from_an_empty_editor() {
        let mut missing = Model::default();
        missing.nodes.get_mut(WINDOW).unwrap().states = bits(&[]);
        let fixture = Fixture::new(missing).await;
        let report = read_report(&fixture.session).await;
        assert!(report.context.is_none());
        assert_eq!(report.status, "no-active-window");
        let fixture = Fixture::new(Model {
            text: "".into(),
            caret: 0,
            ..Model::default()
        })
        .await;
        let report = read_report(&fixture.session).await;
        assert!(report.context.is_some());
        assert_eq!(report.status, "empty-field");
    }

    #[tokio::test]
    async fn insertion_requires_a_bound_handoff_but_recognition_remains_independent() {
        let fixture = Fixture::new(Model {
            shell_context: Some((
                "available".into(),
                "Recognition ".into(),
                " context".into(),
                false,
            )),
            ..Model::default()
        })
        .await;
        let recognition = read_report(&fixture.session).await;
        assert!(recognition.context.is_some());
        let shell = crate::dbus::ShellProxy::new(&fixture.session)
            .await
            .unwrap();
        let (report, token) = read_gnome_insertion_report(&shell).await;
        assert_eq!(report.status, "unbound-extension");
        assert!(report.context.is_none());
        assert!(token.is_none());
        // An old extension and a working AT-SPI editor still cannot bind a GNOME
        // paste. Do not silently adapt using either unbound snapshot.
        assert!(fixture.model.lock().unwrap().calls.is_empty());
        fixture.no_text_read();
    }

    #[tokio::test]
    async fn insertion_accepts_only_bounded_context_with_a_token_and_preserves_status() {
        for (reply, expected, usable) in [
            (
                (
                    "available",
                    "Use ".into(),
                    " now".into(),
                    true,
                    "token".into(),
                ),
                "available",
                true,
            ),
            (
                ("empty", "".into(), "".into(), false, "token".into()),
                "empty-field",
                true,
            ),
            (
                (
                    "available",
                    "😀".repeat(256),
                    "x".repeat(256),
                    true,
                    "token".into(),
                ),
                "available",
                true,
            ),
            (
                (
                    "available",
                    "x".repeat(257),
                    "".into(),
                    false,
                    "token".into(),
                ),
                "invalid-handoff",
                false,
            ),
            (
                (
                    "available",
                    "".into(),
                    "x".repeat(65536),
                    false,
                    "token".into(),
                ),
                "invalid-handoff",
                false,
            ),
            (
                ("available", "secret".into(), "".into(), false, "".into()),
                "invalid-handoff",
                false,
            ),
            (
                (
                    "available",
                    "secret".into(),
                    "".into(),
                    false,
                    "t".repeat(129),
                ),
                "invalid-handoff",
                false,
            ),
            (
                (
                    "protected",
                    "secret".into(),
                    "".into(),
                    true,
                    "token".into(),
                ),
                "protected-field",
                false,
            ),
            (
                ("terminal", "secret".into(), "".into(), true, "token".into()),
                "terminal-field",
                false,
            ),
            (
                ("denied", "secret".into(), "".into(), true, "token".into()),
                "denied",
                false,
            ),
            (
                ("changed", "secret".into(), "".into(), true, "token".into()),
                "changed",
                false,
            ),
            (
                ("timeout", "secret".into(), "".into(), true, "token".into()),
                "timeout",
                false,
            ),
            (
                (
                    "unavailable",
                    "secret".into(),
                    "".into(),
                    true,
                    "token".into(),
                ),
                "unavailable",
                false,
            ),
        ] {
            let (status, before, after, replacement, token) = reply;
            let fixture = Fixture::new(Model {
                shell_insertion_context: Some((
                    status.into(),
                    before.clone(),
                    after.clone(),
                    replacement,
                    token,
                )),
                ..Model::default()
            })
            .await;
            let shell = crate::dbus::ShellProxy::new(&fixture.session)
                .await
                .unwrap();
            let (report, token) = read_gnome_insertion_report(&shell).await;
            assert_eq!(
                (report.status, report.source),
                (expected, "gnome-input-method")
            );
            assert_eq!(token.is_some(), usable);
            assert_eq!(report.context.is_some(), usable);
            if let Some(context) = report.context {
                assert_eq!(
                    (context.before, context.after, context.is_replacement),
                    (before, after, replacement)
                );
            }
            assert!(
                fixture.model.lock().unwrap().calls.is_empty(),
                "never fall through to accessibility"
            );
            fixture.no_text_read();
        }
    }

    #[tokio::test]
    async fn insertion_handoff_deadline_returns_conservative_fallback_without_reading_accessibility()
     {
        let fixture = Fixture::new(Model {
            shell_insertion_context: Some((
                "available".into(),
                "late".into(),
                "".into(),
                false,
                "token".into(),
            )),
            slow_shell_insertion: true,
            ..Model::default()
        })
        .await;
        let shell = crate::dbus::ShellProxy::new(&fixture.session)
            .await
            .unwrap();
        let start = Instant::now();
        let (report, token) = read_gnome_insertion_report(&shell).await;
        assert_eq!(report.status, "timeout");
        assert!(report.context.is_none());
        assert!(token.is_none());
        assert!(
            start.elapsed() >= Duration::from_millis(110),
            "deadline must not be an early server failure"
        );
        assert!(start.elapsed() < Duration::from_millis(250));
        assert!(fixture.model.lock().unwrap().calls.is_empty());
    }
}
