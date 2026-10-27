//! Keep large thread-request future construction out of the dispatcher's poll stack.
use std::future::Future;
use std::pin::Pin;

// Box::pin(handler(...)) constructs the handler's future in the caller first.
// Keeping this factory out of line prevents sibling request arms from reserving
// all of their large construction temporaries in the dispatcher poll frame.
#[inline(never)]
pub(super) fn boxed_request_future<F: Future>(make: impl FnOnce() -> F) -> Pin<Box<F>> {
    Box::pin(make())
}
