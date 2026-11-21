use anyhow::Result;
use clap::Parser;
use dambo_core::DamboRuntime;
use dambo_core::auth::QrPollStatus;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(name = "dambo")]
#[command(
    author = "kshava",
    version = "0.1.0",
    about = "Bilibili live danmaku collector, lossless recorder, and traffic-shaping dam node"
)]
struct Args {
    /// Path to the dambo TOML configuration file
    #[arg(short, long, default_value = "dambo_config.toml")]
    config: String,

    /// Optional room ID or live URL override
    #[arg(short, long)]
    room: Option<String>,

    /// Skip interactive terminal QR login check when unauthenticated
    #[arg(long)]
    no_login: bool,
}

async fn run_cli_qr_login(runtime: &DamboRuntime) -> bool {
    let mut qr = match runtime.auth.generate_qr().await {
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
            _ = tokio::signal::ctrl_c() => {
                return false;
            }
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }

        match runtime.auth.poll_qr(&qr.qrcode_key).await {
            Ok(QrPollStatus::Waiting) => {}
            Ok(QrPollStatus::Scanned) => {
                if !notified_scan {
                    println!("📱 已扫码，请在手机端确认登录...");
                    notified_scan = true;
                }
            }
            Ok(QrPollStatus::Expired) => {
                println!("⏳ 二维码已过期，正在自动刷新...");
                match runtime.auth.generate_qr().await {
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

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let runtime = Arc::new(DamboRuntime::from_config_path(&args.config)?);
    let log_level = runtime.config_snapshot().log.level_filter();
    let dep_level = log_level.min(log::LevelFilter::Warn);
    let default_filter = format!(
        "{},dambo_core={},dambo={}",
        dep_level.as_str().to_ascii_lowercase(),
        log_level.as_str().to_ascii_lowercase(),
        log_level.as_str().to_ascii_lowercase()
    );
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&default_filter))
        .init();

    if !args.no_login {
        let should_prompt_qr = if runtime.auth.current_session().is_some() {
            match runtime.auth.validate_session().await {
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

        if should_prompt_qr && !run_cli_qr_login(&runtime).await {
            log::info!("🛑 已取消登录，退出 dambo");
            return Ok(());
        }
    }

    runtime.start().await?;

    if let Some(ref target_room) = args.room {
        let room_id = runtime.switch_room(target_room).await?;
        log::info!("🎯 CLI room override connected: {}", room_id);
    }

    let cfg = runtime.config_snapshot();
    let base_http = format!("http://{}:{}", cfg.server.host, cfg.server.port);
    let base_ws = format!("ws://{}:{}", cfg.server.host, cfg.server.port);

    log::info!("🚀 dambo headless node started:");
    log::info!("   • OBS Overlay     : {}/", base_http);
    log::info!("   • Danmaku Monitor : {}/list", base_http);
    log::info!("   • Channel A       : {}/ws", base_ws);
    log::info!("   • Channel B       : {}/ws/dam", base_ws);

    tokio::signal::ctrl_c().await?;
    log::info!("🛑 Shutting down dambo...");
    runtime.shutdown();
    Ok(())
}
