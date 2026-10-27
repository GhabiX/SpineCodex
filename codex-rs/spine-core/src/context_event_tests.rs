use super::*;
use crate::NodeId;
use crate::NodeStatus;
use pretty_assertions::assert_eq;

#[test]
fn tag_is_size_neutral_and_splice_unifies_structural_operations() {
    let events = [
        ContextEvent::Tag {
            index: 1,
            label: ContextLabel::UserAnchor(2),
        },
        ContextEvent::Splice {
            start: 2,
            delete: 1,
            insert: vec![
                ContextInsert::Existing {
                    cell_id: CellId::new(8),
                    source_index: 2,
                },
                ContextInsert::Synthetic {
                    cell_id: CellId::new(9),
                    item: ContextItem::SyntheticNode {
                        node_id: NodeId::root_epoch(1),
                        summary: "root".to_string(),
                        status: NodeStatus::Live,
                    },
                },
            ],
        },
    ];

    assert_eq!(ContextEvent::resulting_size(4, &events), Ok(5));
}

#[test]
fn invalid_event_ranges_are_rejected_before_handler_preparation() {
    let event = ContextEvent::Splice {
        start: 2,
        delete: 2,
        insert: Vec::new(),
    };

    assert_eq!(
        ContextEvent::resulting_size(3, &[event]),
        Err(ContextEventError::RangeOutOfBounds {
            start: 2,
            delete: 2,
            size: 3,
        })
    );
}
