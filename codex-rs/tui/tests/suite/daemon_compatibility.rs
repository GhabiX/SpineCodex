//! Invocation-specific features stay in the embedded server.

use super::focus_palette::PtyCodex;
use super::focus_palette::write_test_config;
use anyhow::Result;
use pretty_assertions::assert_eq;

#[test]
fn explicit_features_do_not_connect_to_an_implicit_daemon() -> Result<()> {
    for enabled in [false, true] {
        let cwd = codex_utils_cargo_bin::repo_root()?;
        let home = tempfile::tempdir()?;
        write_test_config(home.path(), &cwd)?;
        let socket = codex_app_server_client::app_server_control_socket_path(home.path())?;
        std::fs::create_dir_all(socket.parent().unwrap())?;
        let listener = std::os::unix::net::UnixListener::bind(socket.as_path())?;
        listener.set_nonblocking(true)?;
        let flag = format!("features.api_key_model_discovery={enabled}");
        let mut terminal = PtyCodex::start(&cwd, home, &["-c", &flag])?;
        terminal.wait_for_startup()?;
        terminal.write_input(b"/status")?;
        terminal.wait_for_screen("show current session configuration")?;
        terminal.read_output(std::time::Duration::from_millis(200))?;
        terminal.write_input(b"\r")?;
        terminal.wait_for_screen("Model:")?;
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
        );
    }
    Ok(())
}
