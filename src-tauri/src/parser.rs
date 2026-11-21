use uuid::Uuid;

use crate::model::{ForwardKind, ForwardSpec, Hop, Msg, TunnelConfig};

/// Parse an ssh command string into a TunnelConfig.
///
/// Handles:
/// - Combined flags: -fNg
/// - -L with space or concatenated: `-L port:host:port` or `-Lport:host:port`
/// - Username containing '@': split on LAST '@' to find host
/// - Optional -p port, -i identity_file, -g bind-all
/// - -J proxy-jump lists: comma-separated `[user@]host[:port]` items, chained
///   before the destination (destination's `:port` / `-p` only affect the
///   last hop).
///
/// Errors are structured `Msg` codes; the frontend locales render them in
/// the user's language.
pub fn parse_ssh_command(raw: &str, name: Option<String>) -> Result<TunnelConfig, Msg> {
    let tokens = shell_words::split(raw).map_err(|e| Msg::ParseTokenizeFailed {
        detail: e.to_string(),
    })?;
    let tokens: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();

    // Skip leading "ssh" token if present
    let start = if tokens.first().map(|t| *t == "ssh").unwrap_or(false) {
        1
    } else {
        0
    };
    let tokens = &tokens[start..];

    let mut forwards: Vec<ForwardSpec> = Vec::new();
    let mut jump_hops: Vec<Hop> = Vec::new();
    let mut bind_all = false;
    let mut identity_file: Option<String> = None;
    let mut jump_port: u16 = 22;
    let mut destination: Option<String> = None;

    // Flags that consume the next token as argument
    const FLAGS_WITH_ARG: &[char] = &[
        'L', 'i', 'p', 'D', 'R', 'E', 'F', 'I', 'J', 'l', 'm', 'o', 'O', 'Q', 'S', 'w', 'b',
        'c', 'W', 'e',
    ];

    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];

        if tok == "--" {
            // Everything after -- is the destination
            i += 1;
            if i < tokens.len() {
                destination = Some(tokens[i].to_string());
            }
            break;
        }

        if !tok.starts_with('-') {
            // Non-flag token = destination
            destination = Some(tok.to_string());
            i += 1;
            continue;
        }

        // Flag token: iterate over characters after '-'
        let flag_chars: Vec<char> = tok[1..].chars().collect();
        let mut j = 0;

        while j < flag_chars.len() {
            let c = flag_chars[j];

            if FLAGS_WITH_ARG.contains(&c) {
                // The rest of this token (after current char) is the arg,
                // OR the next token is the arg
                let rest_of_tok: String = flag_chars[j + 1..].iter().collect();
                let arg: String = if !rest_of_tok.is_empty() {
                    // arg is concatenated: -L10008:host:port
                    // j is now irrelevant since we break below
                    rest_of_tok
                } else {
                    // arg is next token
                    i += 1;
                    if i >= tokens.len() {
                        return Err(Msg::ParseFlagNeedsArg { flag: c.to_string() });
                    }
                    tokens[i].to_string()
                };

                match c {
                    'L' => forwards.push(parse_forward_spec(&arg)?),
                    'D' => forwards.push(parse_dynamic_spec(&arg)?),
                    'R' => forwards.push(parse_remote_spec(&arg)?),
                    'J' => jump_hops.extend(parse_jump_list(&arg)?),
                    'i' => identity_file = Some(arg),
                    'p' => {
                        jump_port = arg.parse().map_err(|_| Msg::ParseInvalidPort {
                            value: arg.clone(),
                        })?
                    }
                    _ => {} // ignore other arg-taking flags
                }
                break; // rest of flag chars were the argument
            } else {
                // No-arg flag
                match c {
                    'g' => bind_all = true,
                    // f=background, N=no-command, T=no-pty, C=compress, q=quiet, v=verbose, etc.
                    _ => {}
                }
                j += 1;
            }
        }

        i += 1;
    }

    let dest = destination.ok_or(Msg::ParseNoDestination)?;

    // Split user@host on LAST '@' — handles usernames like "liu.zy@pg.com"
    let (jump_user, hostport) = match dest.rfind('@') {
        Some(pos) => (dest[..pos].to_string(), dest[pos + 1..].to_string()),
        None => (String::new(), dest),
    };

    let (jump_host, port_override) = parse_hostport(&hostport);
    if let Some(p) = port_override {
        jump_port = p;
    }

    if forwards.is_empty() {
        return Err(Msg::ParseNoForwards);
    }

    // Full hop chain: proxy jumps in command order, then the destination.
    // Jump entries without a user inherit the destination's user (OpenSSH
    // semantics). `-p` and the destination's `:port` only affect the last hop.
    let destination_user = jump_user;
    for hop in &mut jump_hops {
        if hop.user.is_empty() {
            hop.user = destination_user.clone();
        }
    }
    jump_hops.push(Hop {
        user: destination_user,
        host: jump_host,
        port: jump_port,
    });
    let hops = jump_hops;
    // The legacy mirror always reflects the first hop (the entry machine).
    let first = hops.first().expect("hops always contains the destination");

    let display_name = name.unwrap_or_else(|| {
        if forwards.len() == 1 {
            format!("{}:{}", first.host, forwards[0].local_port)
        } else {
            format!("{} ({} forwards)", first.host, forwards.len())
        }
    });

    Ok(TunnelConfig {
        id: Uuid::new_v4().to_string(),
        name: display_name,
        raw_command: raw.to_string(),
        jump_user: first.user.clone(),
        jump_host: first.host.clone(),
        jump_port: first.port,
        hops,
        forwards,
        bind_all,
        identity_file,
        auto_reconnect: true,
        group: None,
        environment: None,
    })
}

/// Parse `[bind:]local_port:remote_host:remote_port`
fn parse_forward_spec(spec: &str) -> Result<ForwardSpec, Msg> {
    // Split into at most 4 parts. Handles:
    //   3 parts: local_port:remote_host:remote_port
    //   4 parts: bind_addr:local_port:remote_host:remote_port
    let parts: Vec<&str> = spec.splitn(4, ':').collect();

    match parts.len() {
        3 => {
            let local_port = parts[0]
                .parse()
                .map_err(|_| Msg::ParseInvalidLocalPort { value: parts[0].into() })?;
            let remote_host = parts[1].to_string();
            let remote_port = parts[2]
                .parse()
                .map_err(|_| Msg::ParseInvalidRemotePort { value: parts[2].into() })?;
            Ok(ForwardSpec {
                local_port,
                remote_host,
                remote_port,
                kind: ForwardKind::Local,
                remote_bind: None,
            })
        }
        4 => {
            // 4 parts: bind_addr:local_port:remote_host:remote_port
            let local_port = parts[1]
                .parse()
                .map_err(|_| Msg::ParseInvalidLocalPort { value: parts[1].into() })?;
            let remote_host = parts[2].to_string();
            let remote_port = parts[3]
                .parse()
                .map_err(|_| Msg::ParseInvalidRemotePort { value: parts[3].into() })?;
            Ok(ForwardSpec {
                local_port,
                remote_host,
                remote_port,
                kind: ForwardKind::Local,
                remote_bind: None,
            })
        }
        _ => Err(Msg::ParseInvalidLSpec { spec: spec.into() }),
    }
}

/// Parse a `-D` dynamic-forward spec: `[bind:]port` → a Dynamic rule whose
/// target is decided per-connection at SOCKS handshake time.
///
/// Supported forms: `-D 1080`, `-D1080`, `-D localhost:1080`, `-D [::1]:1080`.
/// If the part after the last ':' parses as a port the prefix is a bind
/// address and is ignored (mirroring how `-L` discards its bind segment);
/// otherwise the whole spec must be a bare port.
fn parse_dynamic_spec(spec: &str) -> Result<ForwardSpec, Msg> {
    let port_str = match spec.rfind(':') {
        Some(pos) => match spec[pos + 1..].parse::<u16>() {
            Ok(_) => &spec[pos + 1..],
            Err(_) => spec,
        },
        None => spec,
    };
    let port = port_str
        .parse()
        .map_err(|_| Msg::ParseInvalidDSpec { spec: spec.into() })?;
    Ok(ForwardSpec::dynamic(port))
}

/// Parse a `-R` remote-forward spec: `[bind:]listen_port:target_host:target_port`.
///
/// - 3 parts: `listen_port:target_host:target_port`
/// - 4 parts: `bind:listen_port:target_host:target_port`
///
/// A listen port of 0 is valid (the server picks one at connect time). The
/// bind segment is kept as-is (`None` when absent, `Some("")` for a leading
/// colon) — the caller maps `None` to the safe "localhost" default.
fn parse_remote_spec(spec: &str) -> Result<ForwardSpec, Msg> {
    let parts: Vec<&str> = spec.splitn(4, ':').collect();
    let (bind, listen_port_str, host, target_port_str) = match parts.len() {
        3 => (None, parts[0], parts[1], parts[2]),
        4 => (Some(parts[0].to_string()), parts[1], parts[2], parts[3]),
        _ => return Err(Msg::ParseInvalidRSpec { spec: spec.into() }),
    };
    let listen_port = listen_port_str
        .parse()
        .map_err(|_| Msg::ParseInvalidRListenPort { value: listen_port_str.into() })?;
    if host.is_empty() {
        return Err(Msg::ParseREmptyTarget { spec: spec.into() });
    }
    let target_port = target_port_str
        .parse()
        .map_err(|_| Msg::ParseInvalidRTargetPort { value: target_port_str.into() })?;
    Ok(ForwardSpec::remote(listen_port, host, target_port, bind))
}

/// Parse a `-J` proxy-jump list: comma-separated `[user@]host[:port]` items.
/// Userless entries are filled in later from the destination user; a missing
/// port defaults to 22. `none` and other OpenSSH extensions are not supported.
fn parse_jump_list(spec: &str) -> Result<Vec<Hop>, Msg> {
    let mut hops = Vec::new();
    for item in spec.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err(Msg::ParseJEmptyHop { spec: spec.into() });
        }
        // Split user@host on the LAST '@' (usernames may contain '@').
        let (user, hostport) = match item.rfind('@') {
            Some(pos) => (item[..pos].to_string(), item[pos + 1..].to_string()),
            None => (String::new(), item.to_string()),
        };
        let (host, port) = parse_hostport(&hostport);
        if host.is_empty() {
            return Err(Msg::ParseJEmptyHost { hop: item.into() });
        }
        hops.push(Hop {
            user,
            host,
            port: port.unwrap_or(22),
        });
    }
    Ok(hops)
}

/// Parse "host" or "host:port". Returns (host, Some(port)) if port is present.
fn parse_hostport(s: &str) -> (String, Option<u16>) {
    if s.starts_with('[') {
        // IPv6: [::1] or [::1]:port
        if let Some(end) = s.find(']') {
            let host = s[1..end].to_string();
            let rest = &s[end + 1..];
            let port = if rest.starts_with(':') {
                rest[1..].parse().ok()
            } else {
                None
            };
            return (host, port);
        }
    }
    // For IPv4 / hostname: only treat last ':segment' as port if segment is numeric
    if let Some(pos) = s.rfind(':') {
        let possible_port = &s[pos + 1..];
        if let Ok(port) = possible_port.parse::<u16>() {
            return (s[..pos].to_string(), Some(port));
        }
    }
    (s.to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_db_tunnel() {
        let cmd = "ssh -fNg -L 10008:b2b-qa-cne2-centrald-rds-mysql-1.mysql.database.chinacloudapi.cn:3306 liu.zy@pg.com@10.125.24.20";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.jump_user, "liu.zy@pg.com");
        assert_eq!(cfg.jump_host, "10.125.24.20");
        assert_eq!(cfg.jump_port, 22);
        assert!(cfg.bind_all);
        assert_eq!(cfg.forwards.len(), 1);

        let f = &cfg.forwards[0];
        assert_eq!(f.local_port, 10008);
        assert_eq!(
            f.remote_host,
            "b2b-qa-cne2-centrald-rds-mysql-1.mysql.database.chinacloudapi.cn"
        );
        assert_eq!(f.remote_port, 3306);
    }

    #[test]
    fn test_parse_vm_tunnel() {
        let cmd = "ssh -fNg -L 10010:10.125.162.245:22 liu.zy@pg.com@10.125.24.20";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.jump_user, "liu.zy@pg.com");
        assert_eq!(cfg.jump_host, "10.125.24.20");
        assert_eq!(cfg.jump_port, 22);
        assert!(cfg.bind_all);
        assert_eq!(cfg.forwards.len(), 1);

        let f = &cfg.forwards[0];
        assert_eq!(f.local_port, 10010);
        assert_eq!(f.remote_host, "10.125.162.245");
        assert_eq!(f.remote_port, 22);
    }

    #[test]
    fn test_parse_custom_port() {
        let cmd = "ssh -p 2222 -L 8080:internal.host:80 user@jumphost.example.com";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.jump_port, 2222);
        assert_eq!(cfg.jump_host, "jumphost.example.com");
        assert_eq!(cfg.jump_user, "user");
        assert_eq!(cfg.forwards[0].local_port, 8080);
        assert_eq!(cfg.forwards[0].remote_host, "internal.host");
        assert_eq!(cfg.forwards[0].remote_port, 80);
    }

    #[test]
    fn test_parse_dynamic_bare_port() {
        let cmd = "ssh -D 1080 user@jump.example.com";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.jump_host, "jump.example.com");
        assert_eq!(cfg.forwards.len(), 1);
        let f = &cfg.forwards[0];
        assert_eq!(f.kind, ForwardKind::Dynamic);
        assert_eq!(f.local_port, 1080);
        // Placeholder fields for Dynamic rules.
        assert_eq!(f.remote_host, "");
        assert_eq!(f.remote_port, 0);
    }

    #[test]
    fn test_parse_dynamic_concatenated() {
        let cmd = "ssh -D1080 user@jump.example.com";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.forwards.len(), 1);
        assert_eq!(cfg.forwards[0].kind, ForwardKind::Dynamic);
        assert_eq!(cfg.forwards[0].local_port, 1080);
    }

    #[test]
    fn test_parse_dynamic_bind_prefix_ignored() {
        for spec in ["localhost:1080", "[::1]:1080"] {
            let cmd = format!("ssh -D {} user@jump.example.com", spec);
            let cfg = parse_ssh_command(&cmd, None).unwrap();
            assert_eq!(cfg.forwards.len(), 1, "spec: {}", spec);
            assert_eq!(cfg.forwards[0].kind, ForwardKind::Dynamic, "spec: {}", spec);
            assert_eq!(cfg.forwards[0].local_port, 1080, "spec: {}", spec);
        }
    }

    #[test]
    fn test_parse_dynamic_invalid_port_rejected() {
        let cmd = "ssh -D notaport user@jump.example.com";
        assert!(parse_ssh_command(cmd, None).is_err());
        // Trailing non-port after ':' also fails (no bare-port fallback shape).
        let cmd2 = "ssh -D host:notaport user@jump.example.com";
        assert!(parse_ssh_command(cmd2, None).is_err());
    }

    #[test]
    fn test_parse_mixed_local_and_dynamic() {
        let cmd = "ssh -L 10008:host:3306 -D 1080 user@jump";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        // Push order follows command order.
        assert_eq!(cfg.forwards.len(), 2);
        let l = &cfg.forwards[0];
        assert_eq!(l.kind, ForwardKind::Local);
        assert_eq!(l.local_port, 10008);
        assert_eq!(l.remote_host, "host");
        assert_eq!(l.remote_port, 3306);
        let d = &cfg.forwards[1];
        assert_eq!(d.kind, ForwardKind::Dynamic);
        assert_eq!(d.local_port, 1080);
        assert_eq!(d.remote_host, "");
        assert_eq!(d.remote_port, 0);
    }

    // ─── -R (remote forwards, B4) ────────────────────────────────────────────

    #[test]
    fn test_parse_remote_bare() {
        let cmd = "ssh -R 8080:localhost:80 user@jump.example.com";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.forwards.len(), 1);
        let f = &cfg.forwards[0];
        assert_eq!(f.kind, ForwardKind::Remote);
        assert_eq!(f.local_port, 8080);
        assert_eq!(f.listen_port(), 8080);
        assert_eq!(f.target(), Some(("localhost", 80)));
        assert_eq!(f.remote_bind, None);
        assert!(!f.has_local_listener());
    }

    #[test]
    fn test_parse_remote_with_bind() {
        let cmd = "ssh -R 0.0.0.0:8080:10.0.0.5:80 user@jump";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        let f = &cfg.forwards[0];
        assert_eq!(f.kind, ForwardKind::Remote);
        assert_eq!(f.remote_bind.as_deref(), Some("0.0.0.0"));
        assert_eq!(f.local_port, 8080);
        assert_eq!(f.target(), Some(("10.0.0.5", 80)));
    }

    #[test]
    fn test_parse_remote_empty_bind_prefix() {
        // A leading colon keeps the empty bind segment verbatim (OpenSSH
        // semantics: bind every interface).
        let cmd = "ssh -R :8080:host:80 user@jump";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        let f = &cfg.forwards[0];
        assert_eq!(f.remote_bind.as_deref(), Some(""));
        assert_eq!(f.local_port, 8080);
        assert_eq!(f.target(), Some(("host", 80)));
    }

    #[test]
    fn test_parse_remote_zero_port() {
        let cmd = "ssh -R 0:host:80 user@jump";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        let f = &cfg.forwards[0];
        assert_eq!(f.kind, ForwardKind::Remote);
        assert_eq!(f.local_port, 0);
    }

    #[test]
    fn test_parse_remote_concatenated() {
        let cmd = "ssh -R8080:host:80 user@jump";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.forwards.len(), 1);
        assert_eq!(cfg.forwards[0].kind, ForwardKind::Remote);
        assert_eq!(cfg.forwards[0].local_port, 8080);
        assert_eq!(cfg.forwards[0].target(), Some(("host", 80)));
    }

    #[test]
    fn test_parse_remote_empty_host_rejected() {
        assert!(parse_ssh_command("ssh -R 8080::80 user@jump", None).is_err());
        assert!(parse_ssh_command("ssh -R notaport:host:80 user@jump", None).is_err());
        assert!(parse_ssh_command("ssh -R 8080:host:notaport user@jump", None).is_err());
    }

    #[test]
    fn test_parse_mixed_all_kinds() {
        let cmd = "ssh -L 10008:host:3306 -D 1080 -R 8080:localhost:80 u@j";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        // Push order follows command order.
        assert_eq!(cfg.forwards.len(), 3);
        assert_eq!(cfg.forwards[0].kind, ForwardKind::Local);
        assert_eq!(cfg.forwards[1].kind, ForwardKind::Dynamic);
        assert_eq!(cfg.forwards[2].kind, ForwardKind::Remote);
        assert_eq!(cfg.forwards[2].local_port, 8080);
        assert_eq!(cfg.forwards[2].target(), Some(("localhost", 80)));
    }

    // ─── -J (proxy jump chains, C4) ──────────────────────────────────────────

    fn hop(user: &str, host: &str, port: u16) -> Hop {
        Hop {
            user: user.into(),
            host: host.into(),
            port,
        }
    }

    #[test]
    fn test_parse_jump_full_form() {
        let cmd = "ssh -J u1@h1:2222,u2@h2 -L 10008:t:3306 u3@h3";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(
            cfg.hops,
            vec![hop("u1", "h1", 2222), hop("u2", "h2", 22), hop("u3", "h3", 22)]
        );
        // The legacy mirror always reflects the first (entry) hop.
        assert_eq!(cfg.jump_user, "u1");
        assert_eq!(cfg.jump_host, "h1");
        assert_eq!(cfg.jump_port, 2222);
    }

    #[test]
    fn test_parse_jump_concatenated() {
        let cmd = "ssh -Ju1@h1,u2@h2 -L 10008:t:3306 u3@h3";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.hops.len(), 3);
        assert_eq!(cfg.hops[0], hop("u1", "h1", 22));
        assert_eq!(cfg.hops[1], hop("u2", "h2", 22));
    }

    #[test]
    fn test_parse_jump_user_inherited() {
        let cmd = "ssh -J h1,u2@h2 -L 10008:t:3306 u3@h3";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.hops[0], hop("u3", "h1", 22));
        assert_eq!(cfg.hops[1], hop("u2", "h2", 22));
        assert_eq!(cfg.hops[2], hop("u3", "h3", 22));
    }

    #[test]
    fn test_parse_jump_single_hop() {
        let cmd = "ssh -J u1@h1 -L 10008:t:3306 u2@h2";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.hops.len(), 2);
        assert_eq!(cfg.hops[0], hop("u1", "h1", 22));
        assert_eq!(cfg.hops[1], hop("u2", "h2", 22));
    }

    #[test]
    fn test_parse_jump_repeated_appends() {
        let cmd = "ssh -J u1@h1 -J u2@h2 -L 10008:t:3306 u3@h3";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(
            cfg.hops,
            vec![hop("u1", "h1", 22), hop("u2", "h2", 22), hop("u3", "h3", 22)]
        );
    }

    #[test]
    fn test_parse_jump_rejects_empty_item() {
        assert!(
            parse_ssh_command("ssh -J u1@h1,,u2@h2 -L 1:t:2 u3@h3", None).is_err()
        );
    }

    #[test]
    fn test_parse_jump_rejects_empty_host() {
        assert!(parse_ssh_command("ssh -J u1@ -L 1:t:2 u3@h3", None).is_err());
    }

    #[test]
    fn test_parse_jump_with_dynamic_and_remote() {
        let cmd = "ssh -J u1@h1 -D 1080 -R 8080:localhost:80 u3@h3";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.hops.len(), 2);
        assert_eq!(cfg.forwards.len(), 2);
        assert_eq!(cfg.forwards[0].kind, ForwardKind::Dynamic);
        assert_eq!(cfg.forwards[1].kind, ForwardKind::Remote);
    }

    #[test]
    fn test_parse_no_jump_yields_single_hop() {
        let cmd = "ssh -L 10008:host:3306 user@jump";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        assert_eq!(cfg.hops, vec![hop("user", "jump", 22)]);
        assert_eq!(cfg.jump_user, "user");
        assert_eq!(cfg.jump_host, "jump");
        assert_eq!(cfg.jump_port, 22);
    }

    #[test]
    fn test_parse_destination_port_not_applied_to_jump_hops() {
        let cmd = "ssh -p 2222 -J h1 -L 10008:t:3306 u3@h3";
        let cfg = parse_ssh_command(cmd, None).unwrap();

        // Jump hop: inherited user, default port.
        assert_eq!(cfg.hops[0], hop("u3", "h1", 22));
        // -p applies to the destination only.
        assert_eq!(cfg.hops[1], hop("u3", "h3", 2222));
        // Legacy mirror = first hop.
        assert_eq!(cfg.jump_port, 22);
    }
}
