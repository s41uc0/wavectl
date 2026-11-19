use wavectl_client::Client;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wavectl", about = "Priority-ducking audio hub manager")]
struct Cli {
    /// Machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show sources, priorities and ducking state
    Status,
    /// Priority order (first = highest)
    #[command(subcommand)]
    Priority(Priority),
    /// Set a source's duck depth in dB (e.g. -20), or `reset` for the default
    Duck {
        source: String,
        #[arg(allow_hyphen_values = true)]
        db: String,
    },
    /// Set the default duck depth in dB
    DefaultDepth {
        #[arg(allow_hyphen_values = true)]
        db: f64,
    },
    /// Turn ducking back on
    Resume,
    /// Turn ducking off (all sources back to full level)
    Pause,
    /// Forget a source (it is re-added automatically if it reconnects)
    Remove { source: String },
    /// Re-read the config file
    Reload,
    /// List every program/device stream with its volume and output
    Streams,
    /// Set a stream's volume (percent). Stream = id or part of its name/title
    Volume { stream: String, percent: f64 },
    /// Mute or unmute a stream (`off` to unmute)
    Mute { stream: String, state: Option<String> },
    /// Route a stream to an output device (id or name; `default` follows the default output)
    Route { stream: String, output: String },
    /// Send a stream's audio into another program that is recording (`--off` to unlink)
    Send {
        stream: String,
        program: String,
        #[arg(long)]
        off: bool,
    },
    /// Control a stream's media player: play | pause | playpause | next | previous
    Player { stream: String, action: String },
    /// Give a program its own priority tier (stream = id or part of its name)
    Promote { stream: String },
    /// List outputs and recording programs available for routing
    Outputs,
    /// What happens to a source while a higher one plays: duck | pause | mute | nothing (`reset` for default)
    Behavior { source: String, behavior: String },
}

#[derive(Subcommand)]
enum Priority {
    List,
    /// Set the full order, highest first
    Set { sources: Vec<String> },
    Up { source: String },
    Down { source: String },
}

fn bar(db: Option<f32>) -> String {
    let v = db.map_or(0.0, |d| ((d + 60.0) / 60.0).clamp(0.0, 1.0));
    let n = (v * 20.0).round() as usize;
    format!("{}{}", "█".repeat(n), "░".repeat(20 - n))
}

fn find_stream(c: &Client, q: &str) -> Result<wavectl_core::api::StreamView, String> {
    let st = c.state()?;
    let all: Vec<_> = st.sources.into_iter().flat_map(|s| s.streams).collect();
    if let Ok(id) = q.parse::<u32>() {
        if let Some(s) = all.iter().find(|s| s.id == id) {
            return Ok(s.clone());
        }
    }
    let ql = q.to_lowercase();
    let hits: Vec<_> = all.iter().filter(|s| s.name.to_lowercase().contains(&ql) || s.detail.to_lowercase().contains(&ql)).collect();
    match hits.as_slice() {
        [s] => Ok((*s).clone()),
        [] => Err(format!("no stream matching {q:?} (see `wavectl streams`)")),
        many => Err(format!("{q:?} is ambiguous: {}", many.iter().map(|s| format!("{} ({})", s.id, s.detail)).collect::<Vec<_>>().join(", "))),
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let c = Client::connect()?;
    let json = cli.json;
    match cli.cmd {
        Cmd::Status | Cmd::Priority(Priority::List) => {
            let st = c.state()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&st).unwrap());
                return Ok(());
            }
            println!("ducking {}   default depth {} dB", if st.enabled { "on" } else { "OFF" }, st.default_depth_db);
            for (i, s) in st.sources.iter().enumerate() {
                println!(
                    "{}. {:<22} {} {:<7} {:<8} {}{}",
                    i + 1,
                    s.name,
                    bar(s.level_db),
                    s.state,
                    s.behavior,
                    if s.paused {
                        "paused".to_string()
                    } else if s.ducked {
                        format!("ducked {:.0} dB", s.gain_db)
                    } else if s.suppressed {
                        "suppressed".into()
                    } else {
                        "full".into()
                    },
                    if s.nodes.is_empty() { "  (not connected)" } else { "" },
                );
            }
        }
        Cmd::Priority(Priority::Set { sources }) => c.set_priority(sources)?,
        Cmd::Priority(p @ (Priority::Up { .. } | Priority::Down { .. })) => {
            let (name, delta) = match p {
                Priority::Up { source } => (source, -1i32),
                Priority::Down { source } => (source, 1),
                _ => unreachable!(),
            };
            let mut order: Vec<String> = c.state()?.sources.into_iter().map(|s| s.name).collect();
            let i = order.iter().position(|n| n.eq_ignore_ascii_case(&name)).ok_or(format!("no source {name:?}"))?;
            let j = (i as i32 + delta).clamp(0, order.len() as i32 - 1) as usize;
            order.swap(i, j);
            c.set_priority(order)?;
        }
        Cmd::Duck { source, db } => {
            if db == "reset" {
                c.clear_depth(&source)?
            } else {
                let v: f64 = db.parse().map_err(|_| format!("bad dB value {db:?}"))?;
                c.set_depth(&source, v)?
            }
        }
        Cmd::DefaultDepth { db } => c.set_default_depth(db)?,
        Cmd::Resume => c.set_enabled(true)?,
        Cmd::Pause => c.set_enabled(false)?,
        Cmd::Remove { source } => c.remove_source(&source)?,
        Cmd::Reload => c.reload()?,
        Cmd::Behavior { source, behavior } => {
            let b = if behavior == "reset" { "" } else { behavior.as_str() };
            c.set_behavior(&source, b)?
        }
        Cmd::Streams => {
            let st = c.state()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&st).unwrap());
                return Ok(());
            }
            let out_name = |id: Option<u32>| id.and_then(|i| st.outputs.iter().find(|o| o.id == i)).map_or("-".to_string(), |o| o.name.clone());
            for s in &st.sources {
                println!("{}", s.name);
                for x in &s.streams {
                    println!(
                        "  {:>4}  {:<18} {:<34} {:>3.0}%{} L {} R {}  -> {}{}",
                        x.id,
                        x.name,
                        x.detail.chars().take(34).collect::<String>(),
                        x.volume * 100.0,
                        if x.muted { " muted" } else { "" },
                        bar(Some(x.level_l)).chars().take(10).collect::<String>(),
                        bar(Some(x.level_r)).chars().take(10).collect::<String>(),
                        out_name(x.sink_id),
                        if x.sends.is_empty() { String::new() } else { format!("  + sends to {:?}", x.sends) },
                    );
                    if let Some(p) = &x.player {
                        let track = if p.artist.is_empty() { p.title.clone() } else { format!("{} \u{2014} {}", p.title, p.artist) };
                        println!("        {} [{}] {}", p.name, p.status, track);
                    }
                }
            }
        }
        Cmd::Player { stream, action } => {
            let s = find_stream(&c, &stream)?;
            let p = s.player.ok_or(format!("{} has no media player", s.name))?;
            c.player_control(&p.id, &action)?
        }
        Cmd::Promote { stream } => {
            let s = find_stream(&c, &stream)?;
            c.promote_stream(s.id)?
        }
        Cmd::Outputs => {
            let st = c.state()?;
            println!("outputs:");
            for o in &st.outputs {
                println!("  {:>4}  {}{}", o.id, o.name, if o.is_default { "  (default)" } else { "" });
            }
            println!("recording programs:");
            for r in &st.captures {
                println!("  {:>4}  {} {}", r.id, r.name, r.detail);
            }
        }
        Cmd::Volume { stream, percent } => {
            let s = find_stream(&c, &stream)?;
            c.set_stream_volume(s.id, (percent / 100.0).clamp(0.0, 1.5))?
        }
        Cmd::Mute { stream, state } => {
            let s = find_stream(&c, &stream)?;
            c.set_stream_mute(s.id, state.as_deref() != Some("off"))?
        }
        Cmd::Route { stream, output } => {
            let s = find_stream(&c, &stream)?;
            let id = if output == "default" {
                0
            } else {
                let st = c.state()?;
                let ol = output.to_lowercase();
                let hits: Vec<_> = st.outputs.iter().filter(|o| o.id.to_string() == output || o.name.to_lowercase().contains(&ol)).collect();
                match hits.as_slice() {
                    [o] => o.id,
                    [] => return Err(format!("no output matching {output:?} (see `wavectl outputs`)")),
                    _ => return Err(format!("{output:?} matches several outputs")),
                }
            };
            c.route_stream(s.id, id)?
        }
        Cmd::Send { stream, program, off } => {
            let s = find_stream(&c, &stream)?;
            let st = c.state()?;
            let pl = program.to_lowercase();
            let hits: Vec<_> = st.captures.iter().filter(|r| r.id.to_string() == program || r.name.to_lowercase().contains(&pl) || r.detail.to_lowercase().contains(&pl)).collect();
            match hits.as_slice() {
                [r] => c.send_stream(s.id, r.id, !off)?,
                [] => return Err(format!("no recording program matching {program:?} (see `wavectl outputs`)")),
                _ => return Err(format!("{program:?} matches several recording programs")),
            }
        }
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("wavectl: {e}");
        std::process::exit(1);
    }
}
