# Analytics test fixtures

The thread start/resume sample responses in `src/tests/support.rs` model native
sessions without Spine feedback capability. They set the optional
`spine_feedback_enabled` field to `None`, matching the public response's native
shape. This is a fixture migration only; analytics production behavior and
existing event assertions are unchanged.
