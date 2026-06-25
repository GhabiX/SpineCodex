use crate::spine::runtime::tests::assistant_text_item;
use crate::spine::runtime::tests::text_item;

use super::*;

pub(crate) fn assistant_message(text: &str) -> ResponseItem {
    assistant_text_item(text)
}

pub(crate) fn user_message(text: &str) -> ResponseItem {
    text_item(text)
}
