mod command;
mod format;
mod logger;
mod tui;

use crate::format::{
    connect_host, display_listen_host, format_room_info_plain, format_user_info_plain,
    is_wildcard_host,
};
use crate::tui::{TuiChannels, wait_for_shutdown};
use anyhow::Result;
use clap::Parser;
use dambo_core::DamboRuntime;
use dambo_core::auth::QrPollStatus;
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, watch};

#[derive(Parser, Debug)]
#[command(name = "dambo-cli")]
#[command(
    author = "kshava",
    version,
    about = "Bilibili live danmaku collector, lossless recorder, and interactive CLI"
)]
struct Args {
    /// Path to the dambo TOML configuration file
    #[arg(short, long, default_value = "etc/config.toml")]
    config: String,

    /// Optional room ID or live URL override
    #[arg(short, long)]
    room: Option<String>,

    /// Skip interactive terminal QR login check when unauthenticated
    #[arg(long)]
    no_login: bool,

    /// Enable sidecar raw Bilibili packet capture (*_raw.jsonl with pb_decoded/dm_v2_decoded)
    #[arg(long)]
    record_raw: bool,

    /// Disable interactive TUI and run in headless logging mode
    #[arg(long)]
    headless: bool,
}

async fn next_os_shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            res = tokio::signal::ctrl_c() => res?,
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }
    Ok(())
}

fn spawn_shutdown_watcher(shutdown_tx: watch::Sender<bool>) {
    tokio::spawn(async move {
        if let Err(e) = next_os_shutdown_signal().await {
            log::warn!("Failed to listen for shutdown signal: {}", e);
            return;
        }
        let _ = shutdown_tx.send(true);

        // Second Ctrl+C / SIGTERM force-exits if an external blocking call stalls.
        if next_os_shutdown_signal().await.is_ok() {
            log::warn!("⚠️ 收到第二次中断信号，强制退出 dambo-cli");
            std::process::exit(130);
        }
    });
}

async fn run_cli_qr_login(runtime: &DamboRuntime, shutdown_rx: &mut watch::Receiver<bool>) -> bool {
    let qr_res = tokio::select! {
        _ = wait_for_shutdown(shutdown_rx) => return false,
        res = runtime.auth.generate_qr() => res,
    };
    let mut qr = match qr_res {
        Ok(q) => q,
        Err(e) => {
            log::warn!("⚠️ 获取 B 站登录二维码失败 ({}), 将以游客模式继续", e);
            return true;
        }
    };

    println!("\n{}", qr.terminal_qr);
    println!("请使用哔哩哔哩手机客户端扫描上方二维码登录（按 Ctrl+C 取消）...\n");

    let mut notified_scan = false;
    loop {
        tokio::select! {
            _ = wait_for_shutdown(shutdown_rx) => {
                return false;
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }

        let poll_res = tokio::select! {
            _ = wait_for_shutdown(shutdown_rx) => return false,
            res = runtime.auth.poll_qr(&qr.qrcode_key) => res,
        };
        match poll_res {
            Ok(QrPollStatus::Waiting) => {}
            Ok(QrPollStatus::Scanned) => {
                if !notified_scan {
                    println!("📱 已扫码，请在手机端确认登录...");
                    notified_scan = true;
                }
            }
            Ok(QrPollStatus::Expired) => {
                println!("⏳ 二维码已过期，正在自动刷新...");
                let fresh_res = tokio::select! {
                    _ = wait_for_shutdown(shutdown_rx) => return false,
                    res = runtime.auth.generate_qr() => res,
                };
                match fresh_res {
                    Ok(fresh) => {
                        qr = fresh;
                        notified_scan = false;
                        println!("\n{}\n请重新扫描二维码登录...", qr.terminal_qr);
                    }
                    Err(e) => {
                        log::warn!("⚠️ 刷新二维码失败 ({}), 将以游客模式继续", e);
                        return true;
                    }
                }
            }
            Ok(QrPollStatus::Success(_)) => {
                let state = runtime.auth.current_state();
                println!("✅ 登录成功！{}", format_user_info_plain(&state));
                return true;
            }
            Ok(QrPollStatus::Error(msg)) => {
                log::warn!("⚠️ 扫码登录失败 ({}), 将以游客模式继续", msg);
                return true;
            }
            Err(e) => {
                log::warn!("⚠️ 轮询扫码状态失败 ({}), 将以游客模式继续", e);
                return true;
            }
        }
    }
}

async fn run_headless(
    runtime: Arc<DamboRuntime>,
    mut room_rx: watch::Receiver<dambo_core::model::RoomStateSnapshot>,
    mut event_rx: broadcast::Receiver<dambo_core::model::ChannelBMessage>,
    shutdown_rx: &mut watch::Receiver<bool>,
) {
    let cfg = runtime.config_snapshot();
    let listen_host = display_listen_host(&cfg.server.host);
    let url_host = if is_wildcard_host(&cfg.server.host) {
        connect_host(&cfg.server.host)
    } else {
        listen_host.as_str()
    };
    let base_http = format!("http://{}:{}", url_host, cfg.server.port);
    let base_ws = format!("ws://{}:{}", url_host, cfg.server.port);

    log::info!("🚀 dambo-cli headless node started:");
    log::info!("   • Listen Address  : {}:{}", listen_host, cfg.server.port);
    log::info!("   • Control Panel   : {}/", base_http);
    log::info!("   • OBS Overlay     : {}/default/", base_http);
    log::info!("   • Danmaku Monitor : {}/list/", base_http);
    log::info!("   • Plugin Manage   : {}/<plugin>/m", base_http);
    log::info!("   • Channel A       : {}/ws", base_ws);
    log::info!("   • Channel B       : {}/ws/event", base_ws);

    let lan_payload = runtime.lan_urls(None);
    for line in crate::format::format_lan_info_plain(&lan_payload, 0) {
        log::info!("{}", line);
    }

    let mut announced_room_id: Option<u64> = None;
    let initial_state = room_rx.borrow().clone();
    if initial_state.room_id > 0 && initial_state.connected {
        let info = runtime.hub.room_info();
        println!("{}", format_room_info_plain(&info, &initial_state));
        announced_room_id = Some(initial_state.room_id);
    }

    loop {
        tokio::select! {
            _ = wait_for_shutdown(shutdown_rx) => break,
            res = room_rx.changed() => {
                if res.is_err() {
                    break;
                }
                let state = room_rx.borrow().clone();
                if state.room_id == 0 {
                    announced_room_id = None;
                } else if state.connected && announced_room_id != Some(state.room_id) {
                    let info = runtime.hub.room_info();
                    println!("{}", format_room_info_plain(&info, &state));
                    announced_room_id = Some(state.room_id);
                }
            }
            res = event_rx.recv() => {
                if let Ok(dambo_core::model::ChannelBMessage::RoomInfo(_)) = res {
                    let state = runtime.hub.room_state();
                    if state.room_id > 0 && state.connected && announced_room_id != Some(state.room_id) {
                        let info = runtime.hub.room_info();
                        println!("{}", format_room_info_plain(&info, &state));
                        announced_room_id = Some(state.room_id);
                    }
                }
            }
        }
    }
}

async fn async_main(args: Args, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
    let runtime = Arc::new(DamboRuntime::from_config_path(&args.config)?);
    let log_level = runtime.config_snapshot().log.level_filter();
    let logger_handle = logger::init_logger(log_level)?;

    if !args.no_login {
        let should_prompt_qr = if runtime.auth.current_session().is_some() {
            let val_res = tokio::select! {
                _ = wait_for_shutdown(&mut shutdown_rx) => {
                    log::info!("🛑 已取消登录，退出 dambo-cli");
                    return Ok(());
                }
                res = runtime.auth.validate_session() => res,
            };
            match val_res {
                Ok(state) if state.is_login => {
                    println!("{}", format_user_info_plain(&state));
                    false
                }
                Ok(_) => {
                    log::warn!("⚠️ 本地 B 站会话已过期，需重新扫码登录");
                    true
                }
                Err(e) => {
                    log::warn!("⚠️ 启动校验登录态失败 ({:#}), 将直接尝试连接", e);
                    false
                }
            }
        } else {
            true
        };

        if should_prompt_qr && !run_cli_qr_login(&runtime, &mut shutdown_rx).await {
            log::info!("🛑 已取消登录，退出 dambo-cli");
            return Ok(());
        }
    } else {
        println!("{}", format_user_info_plain(&runtime.auth.current_state()));
    }

    if args.record_raw {
        runtime.set_record_raw_override(true);
    }

    let ws_rx = runtime.hub.subscribe_ws();
    let room_rx = runtime.room_state_rx();
    let auth_rx = runtime.auth_state_rx();
    let event_rx = runtime.subscribe_event();

    let start_res = tokio::select! {
        _ = wait_for_shutdown(&mut shutdown_rx) => {
            log::info!("🛑 Shutting down dambo-cli...");
            runtime.shutdown();
            return Ok(());
        }
        res = runtime.start_with_room_override(args.room.as_deref()) => res,
    };
    if let Err(err) = start_res {
        if let Some(addr) = DamboRuntime::is_port_in_use_error(&err) {
            log::error!(
                "❌ 端口 {} 已被占用（可能已有 dambo / dambo-ui 正在运行），dambo-cli 退出。",
                addr
            );
            eprintln!(
                "❌ 无法启动 dambo-cli：端口 {} 已被占用（可能已有旧版 Dambo 或 dambo-ui 正在运行），请关闭占用进程或修改配置中的 [server].port。",
                addr
            );
        } else {
            log::error!("❌ 启动 dambo-cli 失败: {:#}", err);
            eprintln!("❌ 启动 dambo-cli 失败: {:#}", err);
        }
        runtime.shutdown();
        return Err(err);
    }

    let use_tui =
        !args.headless && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if use_tui {
        let channels = TuiChannels {
            ws_rx,
            room_rx,
            auth_rx,
            event_rx,
            logger_handle,
        };
        tui::run_tui(
            runtime.clone(),
            channels,
            &mut shutdown_rx,
            |rt, mut rx| async move { run_cli_qr_login(&rt, &mut rx).await },
        )
        .await?;
    } else {
        run_headless(runtime.clone(), room_rx, event_rx, &mut shutdown_rx).await;
    }

    log::info!("🛑 Shutting down dambo-cli...");
    runtime.shutdown();
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let tokio_rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let res = tokio_rt.block_on(async move {
        spawn_shutdown_watcher(shutdown_tx);
        async_main(args, shutdown_rx).await
    });

    // Bound runtime teardown so external blocking syscalls (e.g. blivedm DNS/TCP)
    // cannot hang Ctrl-C exit after DamboRuntime::shutdown() has flushed sessions.
    tokio_rt.shutdown_timeout(Duration::from_secs(1));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_binary_name_and_shutdown_watch() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (tx, mut rx) = watch::channel(false);
            let waiter = tokio::spawn(async move {
                wait_for_shutdown(&mut rx).await;
            });
            let _ = tx.send(true);
            tokio::time::timeout(Duration::from_millis(200), waiter)
                .await
                .unwrap()
                .unwrap();
        });
    }

    #[test]
    fn test_cli_args_parsing() {
        let args = Args::try_parse_from([
            "dambo-cli",
            "--config",
            "custom.toml",
            "--room",
            "21452505",
            "--no-login",
            "--record-raw",
            "--headless",
        ])
        .unwrap();
        assert_eq!(args.config, "custom.toml");
        assert_eq!(args.room.as_deref(), Some("21452505"));
        assert!(args.no_login);
        assert!(args.record_raw);
        assert!(args.headless);
    }
}
