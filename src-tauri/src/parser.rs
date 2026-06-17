use anyhow::{anyhow, bail, Result};
use uuid::Uuid;

use crate::model::{ForwardSpec, TunnelConfig};

/// Parse an ssh command string into a TunnelConfig.
///
/// Handles:
/// - Combined flags: -fNg
/// - -L with space or concatenated: `-L port:host:port` or `-Lport:host:port`
/// - Username containing '@': split on LAST '@' to find host
/// - Optional -p port, -i identity_file, -g bind-all
///
/// Does NOT implement ProxyJump (-J). Input commands use direct single-hop -L.
pub fn parse_ssh_command(raw: &str, name: Option<String>) -> Result<TunnelConfig> {
    let tokens = shell_words::split(raw)?;
    let tokens: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();

    // Skip leading "ssh" token if present
    let start = if tokens.first().map(|t| *t == "ssh").unwrap_or(false) {
        1
    } else {
        0
    };
    let tokens = &tokens[start..];

    let mut forwards: Vec<ForwardSpec> = Vec::new();
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
                        bail!("Flag -{} requires an argument", c);
                    }
                    tokens[i].to_string()
                };

                match c {
                    'L' => forwards.push(parse_forward_spec(&arg)?),
                    'i' => identity_file = Some(arg),
                    'p' => {
                        jump_port = arg
                            .parse()
                            .map_err(|_| anyhow!("Invalid port: {}", arg))?
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

    let dest = destination.ok_or_else(|| anyhow!("No SSH destination found in command"))?;

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
        bail!("No -L local port forwarding found in command");
    }

    let display_name = name.unwrap_or_else(|| {
        if forwards.len() == 1 {
            format!("{}:{}", jump_host, forwards[0].local_port)
        } else {
            format!("{} ({} forwards)", jump_host, forwards.len())
        }
    });

    Ok(TunnelConfig {
        id: Uuid::new_v4().to_string(),
        name: display_name,
        raw_command: raw.to_string(),
        jump_user,
        jump_host,
        jump_port,
        forwards,
        bind_all,
        identity_file,
        auto_reconnect: true,
        group: None,
        environment: None,
    })
}

/// Parse `[bind:]local_port:remote_host:remote_port`
fn parse_forward_spec(spec: &str) -> Result<ForwardSpec> {
    // Split into at most 4 parts. Handles:
    //   3 parts: local_port:remote_host:remote_port
    //   4 parts: bind_addr:local_port:remote_host:remote_port
    let parts: Vec<&str> = spec.splitn(4, ':').collect();

    match parts.len() {
        3 => {
            let local_port = parts[0]
                .parse()
                .map_err(|_| anyhow!("Invalid local port in -L: {}", parts[0]))?;
            let remote_host = parts[1].to_string();
            let remote_port = parts[2]
                .parse()
                .map_err(|_| anyhow!("Invalid remote port in -L: {}", parts[2]))?;
            Ok(ForwardSpec {
                local_port,
                remote_host,
                remote_port,
            })
        }
        4 => {
            // 4 parts: bind_addr:local_port:remote_host:remote_port
            let local_port = parts[1]
                .parse()
                .map_err(|_| anyhow!("Invalid local port in -L: {}", parts[1]))?;
            let remote_host = parts[2].to_string();
            let remote_port = parts[3]
                .parse()
                .map_err(|_| anyhow!("Invalid remote port in -L: {}", parts[3]))?;
            Ok(ForwardSpec {
                local_port,
                remote_host,
                remote_port,
            })
        }
        _ => bail!("Invalid -L spec (expected [bind:]lport:rhost:rport): {}", spec),
    }
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
}
