//! Which school's Canvas canter opens: asked once, saved, and changed with
//! `canter --school`.
//!
//! People know their school by the address in their browser or by a login
//! link, not by an origin. What they paste can be the Canvas address, a link
//! on the Canvas site (a specific `/login/...` page some schools need), or a
//! link on the school's own sign-on site. Only the Canvas site says which
//! site canter reads, so each address is asked whether it is Canvas before it
//! is saved, and a sign-on link is kept as the page sign-in starts on.

use crate::config::{DEFAULT_ORIGIN, parse_origin};
use crate::transport::{HttpRequest, Method, Transport};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const FILE: &str = "school.json";

/// The saved choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct School {
    /// `https://host` of the Canvas site.
    pub origin: String,
    /// Where sign-in starts, when not the Canvas site's own login page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_url: Option<String>,
}

impl School {
    pub fn uf() -> Self {
        School {
            origin: DEFAULT_ORIGIN.to_string(),
            login_url: None,
        }
    }
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// The saved school, if one has been chosen and the file still makes sense.
pub fn load(data_dir: &Path) -> Option<School> {
    let school: School = serde_json::from_slice(&std::fs::read(path(data_dir)).ok()?).ok()?;
    parse_origin(&school.origin).ok()?;
    if let Some(link) = &school.login_url {
        parse_address(link).ok()?;
    }
    Some(school)
}

pub fn save(data_dir: &Path, school: &School) -> Result<(), String> {
    let fail = |_| "Could not save the school.".to_string();
    std::fs::create_dir_all(data_dir).map_err(fail)?;
    let body =
        serde_json::to_vec_pretty(school).map_err(|_| "Could not save the school.".to_string())?;
    std::fs::write(path(data_dir), body).map_err(fail)
}

/// A typed address, checked and split.
#[derive(Debug, PartialEq, Eq)]
pub struct Address {
    pub host: String,
    /// The whole link, when it names a page rather than just a site.
    pub link: Option<String>,
}

/// Read an address the way people paste them: with or without `https://`,
/// with a trailing slash, a path or a query. Only https, and only a plain
/// host: no user name, no port.
pub fn parse_address(input: &str) -> Result<Address, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Nothing was entered.".into());
    }
    if input.to_ascii_lowercase().starts_with("http://") {
        return Err("Use the https:// address.".into());
    }
    let rest = match input.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return Err("That isn't a web address.".into()),
        None => input,
    };
    let rest = rest.split('#').next().unwrap_or_default();
    let (authority, page) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.contains('@') || authority.contains(':') {
        return Err("Use the plain address, with no user name or port.".into());
    }
    let host = parse_origin(&format!("https://{authority}"))
        .map_err(|_| "That doesn't look like a web address.".to_string())?;
    let page = if page.starts_with('?') {
        format!("/{page}")
    } else {
        page.to_string()
    };
    let link = (!page.is_empty() && page != "/").then(|| format!("https://{host}{page}"));
    Ok(Address { host, link })
}

/// What a site said when asked whether it is Canvas.
#[derive(Debug, PartialEq, Eq)]
pub enum Probe {
    Canvas,
    NotCanvas,
    Unreachable,
}

/// Ask a host whether it is Canvas. Canvas answers its API with no session
/// with a 401 and its own JSON (`"status": "unauthenticated"` and an errors
/// list); a sign-on page or any other site answers with a page, a redirect
/// or something else. No cookies are sent.
pub async fn probe(transport: &dyn Transport, host: &str) -> Probe {
    let request = HttpRequest {
        method: Method::Get,
        url: format!("https://{host}/api/v1/users/self"),
        headers: vec![("accept", "application/json".to_string())],
        body: None,
    };
    let Ok(response) = transport.send(request).await else {
        return Probe::Unreachable;
    };
    let body: Option<Value> = serde_json::from_slice(&response.body).ok();
    let canvas = response.status == 401
        && body.is_some_and(|v| {
            v.get("status").and_then(Value::as_str) == Some("unauthenticated")
                || v.get("errors").is_some_and(Value::is_array)
        });
    if canvas {
        Probe::Canvas
    } else {
        Probe::NotCanvas
    }
}

/// Ask for the school until a Canvas site is given. `first` is an address
/// already given on the command line. Returns `None` if input ends.
pub fn choose(first: Option<String>) -> Result<Option<School>, String> {
    let transport = crate::transport::ReqwestTransport::new()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Could not start the async runtime.".to_string())?;
    let ask = |host: &str| {
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(15), probe(&transport, host))
                .await
                .unwrap_or(Probe::Unreachable)
        })
    };
    let mut given = first;
    loop {
        let input = match given.take() {
            Some(input) => input,
            None => {
                println!("Which school's Canvas should canter open?");
                match read_line(
                    "Paste your Canvas address or login link (Enter for University of Florida): ",
                ) {
                    Some(line) => line,
                    None => return Ok(None),
                }
            }
        };
        if input.trim().is_empty() {
            return Ok(Some(School::uf()));
        }
        let address = match parse_address(&input) {
            Ok(address) => address,
            Err(why) => {
                println!("{why}\n");
                continue;
            }
        };
        match ask(&address.host) {
            Probe::Canvas => {
                // A page on the Canvas site is kept only if it is a login page;
                // a course or dashboard link just names the site.
                let login_url = address.link.filter(|link| {
                    link.strip_prefix(&format!("https://{}", address.host))
                        .is_some_and(|page| page.starts_with("/login"))
                });
                return Ok(Some(School {
                    origin: format!("https://{}", address.host),
                    login_url,
                }));
            }
            Probe::Unreachable => {
                println!(
                    "Couldn't reach {}. Check the address and your connection.\n",
                    address.host
                );
            }
            Probe::NotCanvas => {
                let Some(link) = address.link else {
                    println!("{} isn't a Canvas site.\n", address.host);
                    continue;
                };
                println!(
                    "That looks like your school's sign-in page, so canter will start sign-in there.\n\
                     It also needs your Canvas address: the one in your browser when you're on Canvas."
                );
                match canvas_address(&ask)? {
                    Some(origin) => {
                        return Ok(Some(School {
                            origin,
                            login_url: Some(link),
                        }));
                    }
                    None => return Ok(None),
                }
            }
        }
    }
}

/// Ask for the Canvas address alone, after a sign-on link.
fn canvas_address(ask: &dyn Fn(&str) -> Probe) -> Result<Option<String>, String> {
    loop {
        let Some(input) = read_line("Canvas address: ") else {
            return Ok(None);
        };
        let host = match parse_address(&input) {
            Ok(address) => address.host,
            Err(why) => {
                println!("{why}");
                continue;
            }
        };
        match ask(&host) {
            Probe::Canvas => return Ok(Some(format!("https://{host}"))),
            Probe::Unreachable => {
                println!("Couldn't reach {host}. Check the address and your connection.")
            }
            Probe::NotCanvas => println!("{host} isn't a Canvas site."),
        }
    }
}

fn read_line(prompt: &str) -> Option<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::tests::{FakeTransport, json};
    use crate::transport::{HttpResponse, NetError};

    #[test]
    fn addresses_are_read_the_way_people_paste_them() {
        let site = |host: &str| Address {
            host: host.into(),
            link: None,
        };
        assert_eq!(
            parse_address("canvas.school.edu").unwrap(),
            site("canvas.school.edu")
        );
        assert_eq!(
            parse_address(" https://School.Instructure.com/ ").unwrap(),
            site("school.instructure.com")
        );
        assert_eq!(
            parse_address("https://canvas.school.edu/login/saml/2?x=1#top").unwrap(),
            Address {
                host: "canvas.school.edu".into(),
                link: Some("https://canvas.school.edu/login/saml/2?x=1".into()),
            }
        );
        assert!(parse_address("").is_err());
        assert!(parse_address("http://canvas.school.edu").is_err());
        assert!(parse_address("ftp://canvas.school.edu").is_err());
        assert!(parse_address("https://user@canvas.school.edu").is_err());
        assert!(parse_address("https://canvas.school.edu:8443").is_err());
        assert!(parse_address("not a site").is_err());
    }

    #[tokio::test]
    async fn only_canvas_s_own_answer_counts_as_canvas() {
        let canvas = FakeTransport::new(vec![json(
            401,
            r#"{"status":"unauthenticated","errors":[{"message":"user authorization required"}]}"#,
        )]);
        assert_eq!(
            probe(canvas.as_ref(), "canvas.school.edu").await,
            Probe::Canvas
        );
        let request = &canvas.requests.lock().unwrap()[0];
        assert_eq!(request.url, "https://canvas.school.edu/api/v1/users/self");
        assert!(request.headers.iter().all(|(name, _)| *name != "cookie"));

        let sign_on = FakeTransport::new(vec![Ok(HttpResponse {
            status: 302,
            headers: vec![("location".into(), "https://sso.school.edu/login".into())],
            ..Default::default()
        })]);
        assert_eq!(
            probe(sign_on.as_ref(), "sso.school.edu").await,
            Probe::NotCanvas
        );
        // A 401 alone is not enough: it has to be Canvas's JSON.
        let other = FakeTransport::new(vec![json(401, r#"{"message":"no"}"#)]);
        assert_eq!(
            probe(other.as_ref(), "api.example.com").await,
            Probe::NotCanvas
        );
        let down = FakeTransport::new(vec![Err(NetError::Connect)]);
        assert_eq!(
            probe(down.as_ref(), "canvas.school.edu").await,
            Probe::Unreachable
        );
    }

    #[test]
    fn the_choice_is_saved_and_a_damaged_file_is_ignored() {
        let dir = std::env::temp_dir().join(format!("canter-school-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&dir), None);
        let school = School {
            origin: "https://canvas.school.edu".into(),
            login_url: Some("https://sso.school.edu/start?app=canvas".into()),
        };
        save(&dir, &school).unwrap();
        assert_eq!(load(&dir), Some(school));
        std::fs::write(dir.join(FILE), br#"{"origin":"http://canvas.school.edu"}"#).unwrap();
        assert_eq!(load(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
