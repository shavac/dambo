use anyhow::Result;
use clap::Parser;
use dambo_core::DamboRuntime;
use dambo_core::auth::QrPollStatus;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

#[derive(Parser, Debug)]
#[command(name = "dambo-cli")]
#[command(
    author = "kshava",
    version,
    about = "Bilibili live danmaku collector, lossless recorder, and event-shaping node"
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

async fn wait_for_shutdown(rx: &mut watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            return;
        }
    }
}

async fn run_cli_qr_login(runtime: &DamboRuntime, shutdown_rx: &mut watch::Receiver<bool>) -> bool {
    let qr_res = tokio::select! {
        _ = wait_for_shutdown(shutdown_rx) => return false,
        res = runtime.auth.generate_qr() => res,
    };
    let mut qr = match qr_res {
        Ok(q) => q,
        Err(e) => {
            log::warn!("⚠️ 获取 B 站登录二维码失败 ({}), 将以游客模式继续启动", e);
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
                println!(
                    "✅ 登录成功！当前账号: @{} (UID: {})",
                    state.uname, state.mid
                );
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

async fn async_main(args: Args, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
    let runtime = Arc::new(DamboRuntime::from_config_path(&args.config)?);
    let log_level = runtime.config_snapshot().log.level_filter();
    let dep_level = log_level.min(log::LevelFilter::Warn);
    let default_filter = format!(
        "{},dambo_core={},dambo_cli={}",
        dep_level.as_str().to_ascii_lowercase(),
        log_level.as_str().to_ascii_lowercase(),
        log_level.as_str().to_ascii_lowercase()
    );
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&default_filter))
        .init();

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
                    log::info!("✅ 已登录 B 站账号: @{} (UID: {})", state.uname, state.mid);
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
    }

    if args.record_raw {
        runtime.set_record_raw_override(true);
    }

    let start_res = tokio::select! {
        _ = wait_for_shutdown(&mut shutdown_rx) => {
            log::info!("🛑 Shutting down dambo-cli...");
            runtime.shutdown();
            return Ok(());
        }
        res = runtime.start_with_room_override(args.room.as_deref()) => res,
    };
    start_res?;

    let cfg = runtime.config_snapshot();
    let base_http = format!("http://{}:{}", cfg.server.host, cfg.server.port);
    let base_ws = format!("ws://{}:{}", cfg.server.host, cfg.server.port);

    log::info!("🚀 dambo-cli headless node started:");
    log::info!("   • OBS Overlay     : {}/", base_http);
    log::info!("   • Danmaku Monitor : {}/list", base_http);
    log::info!("   • Channel A       : {}/ws", base_ws);
    log::info!("   • Channel B       : {}/ws/event", base_ws);

    wait_for_shutdown(&mut shutdown_rx).await;
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
}
