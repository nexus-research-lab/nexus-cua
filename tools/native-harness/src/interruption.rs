//! Real Windows input interruption. Reports metadata only, never pixels or AX text.

use std::error::Error;
use std::time::{Duration, Instant};

use nexus_cua_protocol::{
    AccessibilityMode, Action, ActionKind, CapabilityManifest, Command, CommandResult, ErrorCode,
    MutationStatus, OpenSessionInput, PerformActionInput, PermissionMode, PointerButton,
    SessionInput,
};
use serde_json::json;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    VK_LBUTTON,
};

use super::{
    Cli, Client, element_screenshot_point, list_windows, observe, perform, select_application,
    select_window, unexpected,
};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(super) enum Mode {
    Close,
    External,
    Physical,
    Idle,
}

#[derive(Debug, clap::Args)]
pub(super) struct Args {
    #[arg(value_enum)]
    mode: Mode,
}

pub(super) async fn validate(
    cli: &Cli,
    client: &mut Client,
    args: &Args,
) -> Result<(), Box<dyn Error>> {
    let discovered = match client.send(Command::DiscoverApplications).await? {
        CommandResult::ApplicationsDiscovered(value) => value,
        other => return Err(unexpected("interruption discovery", &other)),
    };
    let app = select_application(&discovered, &cli.application_match)?;
    let session = match client
        .send(Command::OpenSession(OpenSessionInput {
            manifest: CapabilityManifest {
                mode: PermissionMode::Bounded,
                application_refs: vec![app.discovery_ref.clone()],
                allowed_actions: vec![ActionKind::FocusWindow, ActionKind::PressPoint],
                allow_foreground_input: true,
                restore_foreground_after_input: false,
                allow_display_capture: false,
                allow_clipboard_read: false,
                allow_clipboard_write: false,
                ttl_seconds: 60,
            },
        }))
        .await?
    {
        CommandResult::SessionOpened(value) => value,
        other => return Err(unexpected("interruption session", &other)),
    };
    let id = session.session_id;
    let window = select_window(list_windows(client, &id).await?, &cli.window_title_prefix)?;
    let before = observe(client, &id, &window, false, AccessibilityMode::Disabled).await?;
    perform(
        client,
        &id,
        &window,
        &before,
        Action::FocusWindow,
        ActionKind::FocusWindow,
    )
    .await?;
    let before = observe(client, &id, &window, true, AccessibilityMode::Full).await?;
    let point = element_screenshot_point(&before, "Long Press Target")?;
    let input = PerformActionInput {
        session_id: id.clone(),
        window_ref: window.window_ref.clone(),
        observation_id: before.observation_id,
        observe_after: None,
        action: Action::PressPoint {
            point,
            button: PointerButton::Left,
            duration_ms: 10_000,
        },
    };
    let result: Result<(), Box<dyn Error>> = if matches!(args.mode, Mode::Idle) {
        move_external()?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let result = client.send_outcome(Command::PerformAction(input)).await?;
        if !matches!(&result, Err(error) if error.code == ErrorCode::StaleObservation && error.mutation_status == MutationStatus::NotDispatched)
        {
            return Err(
                "between-action input did not invalidate the old observation before dispatch"
                    .into(),
            );
        }
        let fresh = observe(client, &id, &window, false, AccessibilityMode::Disabled).await?;
        perform(
            client,
            &id,
            &window,
            &fresh,
            Action::FocusWindow,
            ActionKind::FocusWindow,
        )
        .await?;
        Ok(())
    } else {
        let mut control = Client::new(cli)?;
        let action = client.send_outcome(Command::PerformAction(input.clone()));
        let interrupt = async {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !button_held() {
                if Instant::now() >= deadline {
                    return Err("native press did not begin".into());
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let started = Instant::now();
            match args.mode {
                Mode::Close => {
                    control
                        .send(Command::CloseSession(SessionInput {
                            session_id: id.clone(),
                        }))
                        .await?;
                }
                Mode::External => move_external()?,
                Mode::Physical => println!("{{\"physical_input\":\"ready_move_mouse_now\"}}"),
                Mode::Idle => unreachable!(),
            }
            Ok::<_, Box<dyn Error>>(started)
        };
        let (outcome, interrupted_at) = tokio::join!(action, interrupt);
        let elapsed = interrupted_at?.elapsed();
        let expected = if matches!(args.mode, Mode::Close) {
            ErrorCode::SessionUnavailable
        } else {
            ErrorCode::UserActive
        };
        match outcome? {
            Err(error)
                if error.code == expected
                    && error.mutation_status == MutationStatus::Indeterminate => {}
            _ => {
                return Err(
                    "interrupted native dispatch did not preserve the expected indeterminate error"
                        .into(),
                );
            }
        }
        if !matches!(args.mode, Mode::Physical) && elapsed > Duration::from_secs(2) {
            return Err("native interruption exceeded its two-second acceptance bound".into());
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while button_held() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if button_held() {
            return Err("interrupted native action left the mouse button held".into());
        }
        if !matches!(args.mode, Mode::Close) {
            let fresh = observe(client, &id, &window, false, AccessibilityMode::Disabled).await?;
            let blocked = client
                .send_outcome(Command::PerformAction(PerformActionInput {
                    observation_id: fresh.observation_id,
                    action: Action::FocusWindow,
                    ..input
                }))
                .await?;
            if !matches!(blocked, Err(error) if error.code == ErrorCode::UserActive && error.mutation_status == MutationStatus::NotDispatched)
            {
                return Err("fresh observation cleared dispatch-time takeover".into());
            }
        }
        Ok(())
    };
    if !matches!(args.mode, Mode::Close) {
        client
            .send(Command::CloseSession(SessionInput { session_id: id }))
            .await?;
    }
    result?;
    println!(
        "{}",
        json!({"mode": format!("{:?}", args.mode).to_lowercase(), "status": "passed", "button_released": true})
    );
    Ok(())
}

fn button_held() -> bool {
    unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 }
}

fn move_external() -> Result<(), Box<dyn Error>> {
    // One pixel relative movement from this distinct test process. Never use
    // the runtime's own-event marker; this is not physical-device acceptance.
    let event = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 1,
                dwFlags: MOUSEEVENTF_MOVE,
                ..Default::default()
            },
        },
    };
    if unsafe { SendInput(&[event], i32::try_from(size_of::<INPUT>())?) } != 1 {
        return Err("external test input was rejected by Windows".into());
    }
    Ok(())
}
