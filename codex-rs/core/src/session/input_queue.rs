use crate::state::ActiveTurn;
use crate::state::MailboxDeliveryPhase;
use crate::state::TurnState;
use codex_protocol::AgentPath;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::user_input::UserInput;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tokio::sync::watch;

/// Input consumed by a regular turn.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnInput {
    UserInput {
        content: Vec<UserInput>,
        client_id: Option<String>,
    },
    ResponseItem(ResponseItem),
    InterAgentCommunication(InterAgentCommunication),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputQueueActivity {
    Mailbox,
    Steer,
}

/// Turn-local pending input storage owned by the input queue flow.
#[derive(Default)]
pub(crate) struct TurnInputQueue {
    items: Vec<TurnInput>,
}

/// Session-scoped pending input storage and active-turn mailbox delivery coordination.
pub(crate) struct InputQueue {
    activity_tx: watch::Sender<InputQueueActivity>,
    mailbox_pending_mails: Mutex<VecDeque<PendingMailboxCommunication>>,
    mailbox_submissions: Arc<MailboxSubmissionState>,
}

struct PendingMailboxCommunication {
    communication: InterAgentCommunication,
    parent_turn_id: Option<String>,
}

struct MailboxSubmissionState {
    tracker: StdMutex<MailboxSubmissionTracker>,
    revision_tx: watch::Sender<u64>,
}

#[derive(Default)]
struct MailboxSubmissionTracker {
    submissions: HashMap<String, TrackedMailboxSubmission>,
    cancelled_author_subtrees: HashMap<AgentPath, usize>,
}

struct TrackedMailboxSubmission {
    author: AgentPath,
    cancelled: bool,
}

pub(crate) struct MailboxSubmissionRegistration {
    state: Arc<MailboxSubmissionState>,
    submission: Option<(String, AgentPath)>,
}

#[derive(Clone)]
pub(crate) struct MailboxSubmissionCancellation {
    inner: Arc<MailboxSubmissionCancellationInner>,
}

struct MailboxSubmissionCancellationInner {
    state: Arc<MailboxSubmissionState>,
    roots: Vec<AgentPath>,
    active: AtomicBool,
}

impl InputQueue {
    pub(crate) fn new() -> Self {
        let (activity_tx, _) = watch::channel(InputQueueActivity::Mailbox);
        let (revision_tx, _) = watch::channel(0);
        Self {
            activity_tx,
            mailbox_pending_mails: Mutex::new(VecDeque::new()),
            mailbox_submissions: Arc::new(MailboxSubmissionState {
                tracker: StdMutex::new(MailboxSubmissionTracker::default()),
                revision_tx,
            }),
        }
    }

    pub(crate) fn register_mailbox_submission(
        &self,
        submission_id: String,
        author: AgentPath,
    ) -> MailboxSubmissionRegistration {
        self.mailbox_submissions
            .insert(submission_id.clone(), author.clone());
        MailboxSubmissionRegistration {
            state: Arc::clone(&self.mailbox_submissions),
            submission: Some((submission_id, author)),
        }
    }

    pub(crate) fn complete_mailbox_submission(&self, submission_id: &str, author: &AgentPath) {
        self.mailbox_submissions.remove(submission_id, author);
    }

    pub(crate) fn mailbox_submission_cancellation(
        &self,
        roots: &[AgentPath],
    ) -> MailboxSubmissionCancellation {
        MailboxSubmissionCancellation {
            inner: Arc::new(MailboxSubmissionCancellationInner {
                state: Arc::clone(&self.mailbox_submissions),
                roots: deduplicated_paths(roots),
                active: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn take_cancelled_mailbox_submission(
        &self,
        submission_id: &str,
        author: &AgentPath,
    ) -> bool {
        self.mailbox_submissions
            .take_cancelled(submission_id, author)
    }

    pub(crate) async fn wait_for_mailbox_submissions(
        &self,
        predicate: impl Fn(&AgentPath) -> bool,
    ) {
        let mut revision_rx = self.mailbox_submissions.revision_tx.subscribe();
        loop {
            if !self.mailbox_submissions.has_matching(&predicate) {
                return;
            }
            if revision_rx.changed().await.is_err() {
                return;
            }
        }
    }

    pub(crate) async fn subscribe_activity(
        &self,
        turn_state: Option<&Mutex<TurnState>>,
    ) -> (
        watch::Receiver<InputQueueActivity>,
        Option<InputQueueActivity>,
    ) {
        let activity_rx = self.activity_tx.subscribe();
        let has_pending_steer = if let Some(turn_state) = turn_state {
            turn_state.lock().await.pending_input.has_user_input()
        } else {
            false
        };
        let pending_activity = if has_pending_steer {
            Some(InputQueueActivity::Steer)
        } else if self.has_pending_mailbox_items().await {
            Some(InputQueueActivity::Mailbox)
        } else {
            None
        };
        (activity_rx, pending_activity)
    }

    pub(crate) async fn enqueue_mailbox_communication(
        &self,
        communication: InterAgentCommunication,
        parent_turn_id: Option<String>,
    ) {
        self.mailbox_pending_mails
            .lock()
            .await
            .push_back(PendingMailboxCommunication {
                communication,
                parent_turn_id,
            });
        self.activity_tx.send_replace(InputQueueActivity::Mailbox);
    }

    pub(crate) async fn has_pending_mailbox_items(&self) -> bool {
        !self.mailbox_pending_mails.lock().await.is_empty()
    }

    pub(crate) async fn has_trigger_turn_mailbox_items(&self) -> bool {
        self.mailbox_pending_mails
            .lock()
            .await
            .iter()
            .any(|mail| mail.communication.trigger_turn)
    }

    pub(crate) async fn drain_mailbox_input_items(&self) -> (Vec<TurnInput>, Option<String>) {
        let pending_mails = self
            .mailbox_pending_mails
            .lock()
            .await
            .drain(..)
            .collect::<Vec<_>>();
        let parent_turn_id = pending_mails
            .iter()
            .filter(|mail| mail.communication.trigger_turn)
            .map(|mail| mail.parent_turn_id.as_deref())
            .reduce(|expected, candidate| expected.filter(|id| candidate == Some(*id)))
            .and_then(|id| id.filter(|id| !id.trim().is_empty()).map(str::to_string));
        let items = pending_mails
            .into_iter()
            .map(|mail| TurnInput::InterAgentCommunication(mail.communication))
            .collect();
        (items, parent_turn_id)
    }

    pub(crate) async fn extract_mailbox_communications(
        &self,
        mut predicate: impl FnMut(&InterAgentCommunication) -> bool,
    ) -> Vec<InterAgentCommunication> {
        let mut pending = self.mailbox_pending_mails.lock().await;
        let mut retained = VecDeque::with_capacity(pending.len());
        let mut extracted = Vec::new();
        while let Some(mail) = pending.pop_front() {
            if predicate(&mail.communication) {
                extracted.push(mail.communication);
            } else {
                retained.push_back(mail);
            }
        }
        *pending = retained;
        extracted
    }

    pub(crate) async fn turn_state_for_sub_id(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        sub_id: &str,
    ) -> Option<Arc<Mutex<TurnState>>> {
        let active = active_turn.lock().await;
        active.as_ref().and_then(|active_turn| {
            active_turn
                .task
                .as_ref()
                .is_some_and(|task| task.turn_context.sub_id == sub_id)
                .then(|| Arc::clone(&active_turn.turn_state))
        })
    }

    /// Clear any pending waiters and input buffered for the current turn.
    pub(crate) async fn clear_pending(&self, active_turn: &ActiveTurn) {
        let mut turn_state = active_turn.turn_state.lock().await;
        turn_state.clear_pending_waiters();
        turn_state.pending_input.items.clear();
    }

    pub(crate) async fn defer_mailbox_delivery_to_next_turn(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        sub_id: &str,
    ) {
        let turn_state = self.turn_state_for_sub_id(active_turn, sub_id).await;
        let Some(turn_state) = turn_state else {
            return;
        };
        let mut turn_state = turn_state.lock().await;
        // Explicit same-turn work still needs a follow-up. Queue-only child mail does not: keep
        // it pending so task completion records it for the next turn without sampling again.
        if turn_state.pending_input.items.iter().any(|input| {
            !matches!(
                input,
                TurnInput::InterAgentCommunication(communication) if !communication.trigger_turn
            )
        }) {
            return;
        }
        turn_state.set_mailbox_delivery_phase(MailboxDeliveryPhase::NextTurn);
    }

    pub(crate) async fn accept_mailbox_delivery_for_current_turn(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
        sub_id: &str,
    ) {
        let turn_state = self.turn_state_for_sub_id(active_turn, sub_id).await;
        let Some(turn_state) = turn_state else {
            return;
        };
        self.accept_mailbox_delivery_for_turn_state(turn_state.as_ref())
            .await;
    }

    pub(super) async fn accept_mailbox_delivery_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
    ) {
        turn_state
            .lock()
            .await
            .accept_mailbox_delivery_for_current_turn();
    }

    pub(super) async fn extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
        input: Vec<TurnInput>,
    ) {
        {
            let mut turn_state = turn_state.lock().await;
            turn_state.pending_input.items.extend(input);
            turn_state.accept_mailbox_delivery_for_current_turn();
        }
        self.activity_tx.send_replace(InputQueueActivity::Steer);
    }

    pub(crate) async fn extend_pending_input_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
        input: Vec<TurnInput>,
    ) {
        turn_state.lock().await.pending_input.items.extend(input);
    }

    pub(crate) async fn take_pending_input_for_turn_state(
        &self,
        turn_state: &Mutex<TurnState>,
    ) -> Vec<TurnInput> {
        turn_state.lock().await.pending_input.items.split_off(0)
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state updates must remain atomic"
    )]
    pub(crate) async fn get_pending_input(
        &self,
        active_turn: &Mutex<Option<ActiveTurn>>,
    ) -> (Vec<TurnInput>, Option<String>) {
        let (pending_input, accepts_mailbox_delivery) = {
            let mut active = active_turn.lock().await;
            match active.as_mut() {
                Some(active_turn) => {
                    let mut turn_state = active_turn.turn_state.lock().await;
                    let accepts_mailbox_delivery =
                        turn_state.accepts_mailbox_delivery_for_current_turn();
                    let pending_input = if accepts_mailbox_delivery {
                        turn_state.pending_input.items.split_off(0)
                    } else {
                        Vec::new()
                    };
                    (pending_input, accepts_mailbox_delivery)
                }
                None => (Vec::new(), true),
            }
        };
        if !accepts_mailbox_delivery {
            return (pending_input, None);
        }
        let (mailbox_items, parent_turn_id) = self.drain_mailbox_input_items().await;
        if pending_input.is_empty() {
            (mailbox_items, parent_turn_id)
        } else {
            let mut pending_input = pending_input;
            pending_input.extend(mailbox_items);
            (pending_input, parent_turn_id)
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn checks and turn state reads must remain atomic"
    )]
    pub(crate) async fn has_pending_input(&self, active_turn: &Mutex<Option<ActiveTurn>>) -> bool {
        let (has_turn_pending_input, accepts_mailbox_delivery) = {
            let active = active_turn.lock().await;
            match active.as_ref() {
                Some(active_turn) => {
                    let turn_state = active_turn.turn_state.lock().await;
                    (
                        !turn_state.pending_input.items.is_empty(),
                        turn_state.accepts_mailbox_delivery_for_current_turn(),
                    )
                }
                None => (false, true),
            }
        };
        if !accepts_mailbox_delivery {
            return false;
        }
        if has_turn_pending_input {
            return true;
        }
        self.has_pending_mailbox_items().await
    }
}

impl TurnInputQueue {
    fn has_user_input(&self) -> bool {
        self.items
            .iter()
            .any(|input| matches!(input, TurnInput::UserInput { .. }))
    }
}

impl MailboxSubmissionState {
    fn insert(&self, submission_id: String, author: AgentPath) {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cancelled = tracker
            .cancelled_author_subtrees
            .keys()
            .any(|root| path_is_in_subtree(&author, root));
        tracker.submissions.insert(
            submission_id,
            TrackedMailboxSubmission { author, cancelled },
        );
        drop(tracker);
        self.bump_revision();
    }

    fn remove(&self, submission_id: &str, author: &AgentPath) {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tracker
            .submissions
            .get(submission_id)
            .map(|submission| &submission.author)
            != Some(author)
        {
            return;
        }
        tracker.submissions.remove(submission_id);
        drop(tracker);
        self.bump_revision();
    }

    fn has_matching(&self, predicate: &impl Fn(&AgentPath) -> bool) -> bool {
        self.tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .submissions
            .values()
            .any(|submission| !submission.cancelled && predicate(&submission.author))
    }

    fn cancel_author_subtrees(&self, roots: &[AgentPath]) {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for root in roots {
            *tracker
                .cancelled_author_subtrees
                .entry(root.clone())
                .or_default() += 1;
        }
        let cancelled_author_subtrees = tracker
            .cancelled_author_subtrees
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut changed = !roots.is_empty();
        for submission in tracker.submissions.values_mut() {
            if !submission.cancelled
                && cancelled_author_subtrees
                    .iter()
                    .any(|root| path_is_in_subtree(&submission.author, root))
            {
                submission.cancelled = true;
                changed = true;
            }
        }
        drop(tracker);
        if changed {
            self.bump_revision();
        }
    }

    fn release_author_subtrees(&self, roots: &[AgentPath]) {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for root in roots {
            match tracker.cancelled_author_subtrees.get_mut(root) {
                Some(count) if *count > 1 => *count -= 1,
                Some(_) => {
                    tracker.cancelled_author_subtrees.remove(root);
                }
                None => {}
            }
        }
    }

    fn take_cancelled(&self, submission_id: &str, author: &AgentPath) -> bool {
        let mut tracker = self
            .tracker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cancelled = tracker
            .submissions
            .get(submission_id)
            .is_some_and(|submission| submission.author == *author && submission.cancelled);
        if cancelled {
            tracker.submissions.remove(submission_id);
        }
        drop(tracker);
        if cancelled {
            self.bump_revision();
        }
        cancelled
    }

    fn bump_revision(&self) {
        let next = (*self.revision_tx.borrow()).wrapping_add(1);
        self.revision_tx.send_replace(next);
    }
}

fn path_is_in_subtree(candidate: &AgentPath, root: &AgentPath) -> bool {
    candidate == root
        || candidate
            .as_str()
            .strip_prefix(root.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn deduplicated_paths(paths: &[AgentPath]) -> Vec<AgentPath> {
    let mut deduplicated = Vec::new();
    for path in paths {
        if !deduplicated.contains(path) {
            deduplicated.push(path.clone());
        }
    }
    deduplicated
}

impl MailboxSubmissionRegistration {
    pub(crate) fn accepted(mut self) {
        self.submission = None;
    }
}

impl MailboxSubmissionCancellation {
    pub(crate) fn activate(&self) {
        if !self.inner.active.swap(true, Ordering::AcqRel) {
            self.inner.state.cancel_author_subtrees(&self.inner.roots);
        }
    }
}

impl Drop for MailboxSubmissionCancellationInner {
    fn drop(&mut self) {
        if *self.active.get_mut() {
            self.state.release_author_subtrees(&self.roots);
        }
    }
}

impl Drop for MailboxSubmissionRegistration {
    fn drop(&mut self) {
        if let Some((submission_id, author)) = self.submission.take() {
            self.state.remove(&submission_id, &author);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::AgentPath;
    use pretty_assertions::assert_eq;

    fn make_mail(
        author: AgentPath,
        recipient: AgentPath,
        content: &str,
        trigger_turn: bool,
    ) -> InterAgentCommunication {
        InterAgentCommunication::new(
            author,
            recipient,
            Vec::new(),
            content.to_string(),
            trigger_turn,
        )
    }

    #[tokio::test]
    async fn input_queue_notifies_mailbox_subscribers() {
        let input_queue = InputQueue::new();
        let (mut activity_rx, pending_activity) =
            input_queue.subscribe_activity(/*turn_state*/ None).await;
        assert_eq!(pending_activity, None);

        let mail_one = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "one",
            /*trigger_turn*/ false,
        );
        input_queue
            .enqueue_mailbox_communication(mail_one, /*parent_turn_id*/ None)
            .await;
        let mail_two = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "two",
            /*trigger_turn*/ false,
        );
        input_queue
            .enqueue_mailbox_communication(mail_two, /*parent_turn_id*/ None)
            .await;

        activity_rx.changed().await.expect("mailbox update");
        assert_eq!(
            *activity_rx.borrow_and_update(),
            InputQueueActivity::Mailbox
        );
    }

    #[tokio::test]
    async fn input_queue_notifies_steer_subscribers() {
        let input_queue = InputQueue::new();
        let turn_state = Mutex::new(TurnState::default());
        let (mut activity_rx, pending_activity) =
            input_queue.subscribe_activity(Some(&turn_state)).await;
        assert_eq!(pending_activity, None);

        input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                &turn_state,
                vec![TurnInput::UserInput {
                    content: vec![UserInput::Text {
                        text: "steer".to_string(),
                        text_elements: Vec::new(),
                    }],
                    client_id: None,
                }],
            )
            .await;

        activity_rx.changed().await.expect("steer update");
        assert_eq!(*activity_rx.borrow_and_update(), InputQueueActivity::Steer);
    }

    #[tokio::test]
    async fn input_queue_reports_already_pending_steer() {
        let input_queue = InputQueue::new();
        let turn_state = Mutex::new(TurnState::default());
        input_queue
            .extend_pending_input_and_accept_mailbox_delivery_for_turn_state(
                &turn_state,
                vec![TurnInput::UserInput {
                    content: vec![UserInput::Text {
                        text: "already pending".to_string(),
                        text_elements: Vec::new(),
                    }],
                    client_id: None,
                }],
            )
            .await;

        let (_activity_rx, pending_activity) =
            input_queue.subscribe_activity(Some(&turn_state)).await;

        assert_eq!(pending_activity, Some(InputQueueActivity::Steer));
    }

    #[tokio::test]
    async fn input_queue_drains_mailbox_in_delivery_order() {
        let input_queue = InputQueue::new();
        let mail_one = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "one",
            /*trigger_turn*/ false,
        );
        let mail_two = make_mail(
            AgentPath::try_from("/root/worker").expect("agent path"),
            AgentPath::root(),
            "two",
            /*trigger_turn*/ true,
        );

        input_queue
            .enqueue_mailbox_communication(mail_one.clone(), /*parent_turn_id*/ None)
            .await;
        input_queue
            .enqueue_mailbox_communication(mail_two.clone(), /*parent_turn_id*/ None)
            .await;

        assert_eq!(
            input_queue.drain_mailbox_input_items().await.0,
            vec![
                TurnInput::InterAgentCommunication(mail_one),
                TurnInput::InterAgentCommunication(mail_two)
            ]
        );
        assert!(!input_queue.has_pending_mailbox_items().await);
    }

    #[tokio::test]
    async fn input_queue_requires_one_unambiguous_trigger_parent() {
        for (pending_mails, expected_parent_turn_id) in [
            (Vec::new(), None),
            (vec![(false, Some("q"))], None),
            (vec![(true, Some(""))], None),
            (vec![(true, Some("   "))], None),
            (vec![(true, None)], None),
            (vec![(true, Some("a")), (true, Some("b"))], None),
            (vec![(true, Some("a")), (true, None)], None),
            (vec![(true, Some("a")), (true, Some("a"))], Some("a")),
            (vec![(false, Some("q")), (true, Some("a"))], Some("a")),
        ] {
            let input_queue = InputQueue::new();
            for (trigger_turn, parent_turn_id) in pending_mails {
                input_queue
                    .enqueue_mailbox_communication(
                        make_mail(AgentPath::root(), AgentPath::root(), "task", trigger_turn),
                        parent_turn_id.map(str::to_string),
                    )
                    .await;
            }
            let (_, parent_turn_id) = input_queue.drain_mailbox_input_items().await;
            assert_eq!(parent_turn_id.as_deref(), expected_parent_turn_id);
        }
    }

    #[tokio::test]
    async fn input_queue_tracks_pending_trigger_turn_mail() {
        let input_queue = InputQueue::new();

        let queued_mail = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "queued",
            /*trigger_turn*/ false,
        );
        input_queue
            .enqueue_mailbox_communication(queued_mail, /*parent_turn_id*/ None)
            .await;
        assert!(!input_queue.has_trigger_turn_mailbox_items().await);

        let trigger_mail = make_mail(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "wake",
            /*trigger_turn*/ true,
        );
        input_queue
            .enqueue_mailbox_communication(trigger_mail, /*parent_turn_id*/ None)
            .await;
        assert!(input_queue.has_trigger_turn_mailbox_items().await);
    }

    #[tokio::test]
    async fn mailbox_predicate_extraction_preserves_unrelated_order() {
        let input_queue = InputQueue::new();
        let transaction_child = AgentPath::try_from("/root/spawn_a").expect("agent path");
        let unrelated_child = AgentPath::try_from("/root/ordinary").expect("agent path");
        for (author, content) in [
            (unrelated_child.clone(), "before"),
            (transaction_child.clone(), "extract one"),
            (unrelated_child.clone(), "after"),
            (transaction_child.clone(), "extract two"),
        ] {
            input_queue
                .enqueue_mailbox_communication(
                    make_mail(
                        author,
                        AgentPath::root(),
                        content,
                        /*trigger_turn*/ false,
                    ),
                    /*parent_turn_id*/ None,
                )
                .await;
        }

        let extracted = input_queue
            .extract_mailbox_communications(|mail| mail.author == transaction_child)
            .await;
        assert_eq!(
            extracted
                .iter()
                .map(|mail| mail.content.as_str())
                .collect::<Vec<_>>(),
            vec!["extract one", "extract two"]
        );
        let retained = input_queue.drain_mailbox_input_items().await.0;
        assert_eq!(
            retained
                .iter()
                .map(|item| match item {
                    TurnInput::InterAgentCommunication(mail) => mail.content.as_str(),
                    _ => panic!("expected mailbox communication"),
                })
                .collect::<Vec<_>>(),
            vec!["before", "after"]
        );
    }

    #[tokio::test]
    async fn mailbox_submission_waits_for_matching_delivery_only() {
        let input_queue = InputQueue::new();
        let branch = AgentPath::try_from("/root/spawn_a/worker").expect("agent path");
        let unrelated = AgentPath::try_from("/root/ordinary").expect("agent path");
        let branch_submission = input_queue
            .register_mailbox_submission("branch-submission".to_string(), branch.clone());
        branch_submission.accepted();
        let unrelated_submission = input_queue
            .register_mailbox_submission("unrelated-submission".to_string(), unrelated.clone());
        unrelated_submission.accepted();

        let matching = input_queue.wait_for_mailbox_submissions(|author| author == &branch);
        tokio::pin!(matching);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut matching)
                .await
                .is_err()
        );

        input_queue
            .enqueue_mailbox_communication(
                make_mail(
                    branch.clone(),
                    AgentPath::root(),
                    "late branch message",
                    /*trigger_turn*/ false,
                ),
                /*parent_turn_id*/ None,
            )
            .await;
        input_queue.complete_mailbox_submission("branch-submission", &branch);
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut matching)
            .await
            .expect("matching submission should quiesce");
        assert_eq!(
            input_queue
                .extract_mailbox_communications(|mail| mail.author == branch)
                .await
                .len(),
            1
        );
        assert!(!input_queue.has_pending_mailbox_items().await);

        input_queue.complete_mailbox_submission("unrelated-submission", &unrelated);
    }

    #[tokio::test]
    async fn cancelling_mailbox_subtree_tombstones_late_delivery() {
        let input_queue = InputQueue::new();
        let transaction_root = AgentPath::try_from("/root/spawn_a").expect("agent path");
        let descendant = AgentPath::try_from("/root/spawn_a/worker/deep").expect("agent path");
        let unrelated = AgentPath::try_from("/root/spawn_b").expect("agent path");

        let descendant_submission =
            input_queue.register_mailbox_submission("descendant".to_string(), descendant.clone());
        descendant_submission.accepted();
        let unrelated_submission =
            input_queue.register_mailbox_submission("unrelated".to_string(), unrelated.clone());
        unrelated_submission.accepted();

        let matching = input_queue.wait_for_mailbox_submissions(|author| author == &descendant);
        tokio::pin!(matching);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut matching)
                .await
                .is_err()
        );

        let cancellation =
            input_queue.mailbox_submission_cancellation(std::slice::from_ref(&transaction_root));
        cancellation.activate();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut matching)
            .await
            .expect("cancelled subtree submission should no longer block quiescence");
        assert!(input_queue.take_cancelled_mailbox_submission("descendant", &descendant));
        assert!(!input_queue.take_cancelled_mailbox_submission("unrelated", &unrelated));

        let late_submission =
            input_queue.register_mailbox_submission("late".to_string(), descendant.clone());
        late_submission.accepted();
        assert!(input_queue.take_cancelled_mailbox_submission("late", &descendant));

        drop(cancellation);
        let reused_path_submission =
            input_queue.register_mailbox_submission("reused".to_string(), descendant.clone());
        reused_path_submission.accepted();
        assert!(!input_queue.take_cancelled_mailbox_submission("reused", &descendant));
        input_queue.complete_mailbox_submission("reused", &descendant);
        input_queue.complete_mailbox_submission("unrelated", &unrelated);
    }
}
