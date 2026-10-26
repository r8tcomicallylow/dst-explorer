use std::time::Duration;

use percent_encoding::percent_decode_str;
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_TYPE, LOCATION, REFERER, SET_COOKIE};
use reqwest::redirect::Policy;
use scraper::{Html, Selector};

use crate::models::{Album, AlbumDetail, FormatFilter, SearchKind, SITE};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub fn page_client() -> Client {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(15))
        .cookie_store(true)
        .build()
        .expect("HTTP client")
}

#[derive(Clone, Debug)]
pub struct SearchParams {
    pub text: String,
    pub kind: SearchKind,
    pub formats: FormatFilter,
    pub page: usize,
}

#[derive(Clone, Debug)]
pub struct FeedPage {
    pub albums: Vec<Album>,
    pub total_results: Option<usize>,
}

pub fn fetch_latest(client: &Client, page: usize) -> Result<FeedPage, String> {
    let url = format!("{SITE}/?p=home&page={page}");
    let html = get_text(client, &url)?;
    Ok(FeedPage {
        albums: parse_grid(&html),
        total_results: parse_total_results(&html),
    })
}

pub fn fetch_search(client: &Client, params: &SearchParams) -> Result<FeedPage, String> {
    let mut url = reqwest::Url::parse(&format!("{SITE}/")).map_err(|e| e.to_string())?;
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("p", "search");
        q.append_pair("source", "1");
        q.append_pair("type", params.kind.as_str());
        q.append_pair("result", params.text.trim());
        if params.page > 0 {
            q.append_pair("page", &params.page.to_string());
        }
        if params.formats.flac {
            q.append_pair("format0", "on");
        }
        if params.formats.mp3 {
            q.append_pair("format1", "on");
        }
        if params.formats.tta {
            q.append_pair("format2", "on");
        }
        if params.formats.opus {
            q.append_pair("format3", "on");
        }
        if params.formats.ogg {
            q.append_pair("format4", "on");
        }
    }

    let html = get_text(client, url.as_str())?;
    Ok(FeedPage {
        albums: parse_grid(&html),
        total_results: parse_total_results(&html),
    })
}

pub fn fetch_album(client: &Client, album: &Album) -> Result<AlbumDetail, String> {
    let html = get_text(client, &album.page_url())?;
    Ok(parse_album_detail(&html, album))
}

fn get_text(client: &Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .header(REFERER, SITE)
        .send()
        .map_err(|e| format!("Network error: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("DoujinStyle returned HTTP {status}"));
    }
    response
        .text()
        .map_err(|e| format!("Could not read page: {e}"))
}

fn sel(css: &str) -> Selector {
    Selector::parse(css).expect("valid CSS selector")
}

fn text_of(element: &scraper::ElementRef<'_>) -> String {
    element
        .text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn attr_of(element: &scraper::ElementRef<'_>, name: &str) -> Option<String> {
    element.value().attr(name).map(|s| s.to_string())
}

pub fn parse_grid(html: &str) -> Vec<Album> {
    let doc = Html::parse_document(html);
    let box_sel = sel("span.gridBox");
    let title_sel = sel("div.gridDetails > a");
    let tag_sel = sel("a[href*=\"type=tags\"]");
    let format_sel = sel("span[title^=\"Music Format\"]");
    let xfd_sel = sel("a.gridCol2");
    let new_sel = sel(".newTag");
    let link_sel = sel("a[href*=\"p=page\"][href*=\"id=\"]");

    let mut albums = Vec::new();
    for card in doc.select(&box_sel) {
        let link = match card.select(&link_sel).next() {
            Some(link) => link,
            None => continue,
        };
        let href = match attr_of(&link, "href") {
            Some(href) => href,
            None => continue,
        };
        let id = match parse_id_from_href(&href) {
            Some(id) => id,
            None => continue,
        };

        let mut details = card.select(&title_sel);
        let title = details.next().map(|e| text_of(&e)).unwrap_or_default();
        let artist = details.next().map(|e| text_of(&e)).unwrap_or_default();
        if title.is_empty() {
            continue;
        }

        let mut tags = Vec::new();
        for tag in card.select(&tag_sel) {
            let value = text_of(&tag);
            if !value.is_empty() && !tags.iter().any(|t| t == &value) {
                tags.push(value);
            }
        }

        let format = card
            .select(&format_sel)
            .next()
            .and_then(|e| attr_of(&e, "title"))
            .and_then(|t| {
                t.strip_prefix("Music Format:")
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_default();

        let xfd = card
            .select(&xfd_sel)
            .next()
            .and_then(|e| attr_of(&e, "href"))
            .filter(|s| !s.trim().is_empty());

        let is_new = card.select(&new_sel).next().is_some();

        albums.push(Album {
            id,
            title,
            artist,
            tags,
            format,
            xfd,
            is_new,
        });
    }
    albums
}

fn parse_id_from_href(href: &str) -> Option<u64> {
    let mut rest = href.split("id=").nth(1)?;
    loop {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
        rest = rest.get(1..)?;
        if !rest.contains("id=") {
            break;
        }
    }
    None
}

pub fn parse_total_results(html: &str) -> Option<usize> {
    let doc = Html::parse_document(html);
    let heading = sel("h3");
    for node in doc.select(&heading) {
        let text = text_of(&node);
        if let Some(rest) = text.split("Total Results Found:").nth(1) {
            let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
            if let Ok(value) = digits.parse() {
                return Some(value);
            }
        }
    }
    None
}

pub fn parse_album_detail(html: &str, album: &Album) -> AlbumDetail {
    let doc = Html::parse_document(html);
    let mut detail = AlbumDetail::from_album(album.clone());

    if let Some(h2) = doc.select(&sel("mainbar h2")).next() {
        let title = text_of(&h2);
        if !title.is_empty() {
            detail.album.title = title;
        }
    } else if let Some(h2) = doc.select(&sel("h2")).next() {
        let title = text_of(&h2);
        if !title.is_empty() {
            detail.album.title = title;
        }
    }

    let labels = doc
        .select(&sel("mainbar span.pageSpan1"))
        .collect::<Vec<_>>();
    let values = doc
        .select(&sel("mainbar span.pageSpan2"))
        .collect::<Vec<_>>();
    for (label_node, value_node) in labels.iter().zip(values.iter()) {
        let label = text_of(label_node)
            .trim_end_matches(':')
            .trim()
            .to_ascii_lowercase();
        let value = text_of(value_node);
        match label.as_str() {
            "artist" | "artists" => {
                let mut artists = Vec::new();
                for anchor in value_node.select(&sel("a")) {
                    let name = text_of(&anchor);
                    if !name.is_empty() && !artists.iter().any(|a| a == &name) {
                        artists.push(name);
                    }
                }
                if artists.is_empty() && !value.is_empty() {
                    for part in value.split(',') {
                        let part = part.trim();
                        if !part.is_empty() {
                            artists.push(part.to_string());
                        }
                    }
                }
                if !artists.is_empty() {
                    detail.artists = artists;
                }
            }
            "tags" => {
                let mut tags = Vec::new();
                for anchor in value_node.select(&sel("a")) {
                    let tag = text_of(&anchor);
                    if !tag.is_empty() && !tags.iter().any(|t| t == &tag) {
                        tags.push(tag);
                    }
                }
                if tags.is_empty() && !value.is_empty() {
                    for part in value.split(',') {
                        let part = part.trim();
                        if !part.is_empty() {
                            tags.push(part.to_string());
                        }
                    }
                }
                if !tags.is_empty() {
                    detail.tags = tags;
                }
            }
            "xfade" | "x fade" | "crossfade" => {
                if let Some(anchor) = value_node.select(&sel("a")).next() {
                    if let Some(href) = attr_of(&anchor, "href") {
                        if href.starts_with("http") {
                            detail.xfd = Some(href);
                        }
                    }
                }
            }
            "format" => {
                if !value.is_empty() {
                    detail.format = value;
                }
            }
            "date added" => detail.date_added = value,
            "# of downloads" | "downloads" | "number of downloads" => detail.download_count = value,
            "average rating" | "rating" => detail.rating = value,
            "provided by" | "provider" => {
                if !value.is_empty() {
                    detail.provider = Some(value);
                }
            }
            _ => {}
        }
    }

    detail.album.format = detail.format.clone();
    detail.album.tags = detail.tags.clone();
    detail.album.xfd = detail.xfd.clone();
    if let Some(first) = detail.artists.first() {
        detail.album.artist = detail.artists.join(", ");
        let _ = first;
    } else {
        detail.album.artist = detail.album.artist.clone();
    }
    detail
}

#[derive(Clone, Debug)]
pub struct DirectLink {
    pub url: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub enum Resolution {
    Direct(DirectLink),
    Manual { url: String, reason: String },
}

/// Ask DoujinStyle for the external host link, then resolve it to a direct file URL.
pub fn resolve_download(album_id: u64) -> Result<Resolution, String> {
    let client = Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(35))
        .connect_timeout(Duration::from_secs(15))
        .cookie_store(true)
        .redirect(Policy::none())
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .post(format!("{SITE}/"))
        .header(REFERER, format!("{SITE}/?p=page&type=1&id={album_id}"))
        .form(&[
            ("type", "1".to_string()),
            ("id", album_id.to_string()),
            ("source", "0".to_string()),
            ("download_link", String::new()),
        ])
        .send()
        .map_err(|e| format!("Could not contact DoujinStyle: {e}"))?;

    let status = response.status();
    if let Some(location) = response.headers().get(LOCATION) {
        let location = location.to_str().unwrap_or_default().trim();
        if !location.is_empty() {
            let target = if location.starts_with("http") {
                location.to_string()
            } else {
                format!("{SITE}/{location}")
            };
            return resolve_host(&client, &target, 0);
        }
    }

    if status.is_success() {
        let text = response.text().unwrap_or_default();
        if let Some(url) = find_direct_href(&text) {
            return Ok(Resolution::Direct(DirectLink {
                url,
                headers: vec![("Referer".into(), SITE.into())],
            }));
        }
        return Err("DoujinStyle did not return a download link for this album.".into());
    }

    Err(format!(
        "DoujinStyle refused the download request (HTTP {status}). The file may be unavailable."
    ))
}

fn resolve_host(client: &Client, url: &str, depth: usize) -> Result<Resolution, String> {
    if depth > 3 {
        return Ok(Resolution::Manual {
            url: url.to_string(),
            reason: "Too many redirects while resolving this host.".into(),
        });
    }

    let parsed = reqwest::Url::parse(url).map_err(|_| format!("Invalid download URL: {url}"))?;
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();

    // MediaFire: the public file page contains the direct CDN link.
    if host.ends_with("mediafire.com") {
        return resolve_mediafire(client, url);
    }

    if host == "drive.google.com" || host == "docs.google.com" {
        return resolve_google_drive(client, url);
    }

    if host == "www.dropbox.com" || host == "dropbox.com" {
        let mut direct = parsed.clone();
        direct.query_pairs_mut().clear().append_pair("dl", "1");
        return Ok(Resolution::Direct(DirectLink {
            url: direct.to_string(),
            headers: vec![("Referer".into(), SITE.into())],
        }));
    }

    if host == "dropboxusercontent.com" || host.ends_with(".dropboxusercontent.com") {
        return Ok(Resolution::Direct(DirectLink {
            url: url.to_string(),
            headers: vec![("Referer".into(), SITE.into())],
        }));
    }

    if host == "pixeldrain.com" {
        let id = parsed
            .path_segments()
            .and_then(|mut segments| {
                let first = segments.next()?;
                if first == "u" || first == "d" {
                    segments.next().map(|s| s.to_string())
                } else {
                    Some(first.to_string())
                }
            })
            .unwrap_or_default();
        if !id.is_empty() {
            return Ok(Resolution::Direct(DirectLink {
                url: format!("https://pixeldrain.com/api/file/{id}?download"),
                headers: vec![("Referer".into(), "https://pixeldrain.com/".into())],
            }));
        }
    }

    if host == "mega.nz" || host.ends_with(".mega.nz") || host == "mega.co.nz" {
        return Ok(Resolution::Manual {
            url: url.to_string(),
            reason: "MEGA downloads are encrypted and can't be automated. Opening the page in your browser.".into(),
        });
    }

    if host == "drive.usercontent.google.com" || host == "usercontent.google.com" {
        return Ok(Resolution::Direct(DirectLink {
            url: url.to_string(),
            headers: vec![("Referer".into(), "https://drive.google.com/".into())],
        }));
    }

    if looks_like_file(&parsed) {
        return Ok(Resolution::Direct(DirectLink {
            url: url.to_string(),
            headers: vec![("Referer".into(), SITE.into())],
        }));
    }

    // Some hosts give a small landing page with an obvious direct download link.
    let response = client.get(url).header(REFERER, SITE).send();
    if let Ok(response) = response {
        if let Some(location) = response.headers().get(LOCATION) {
            let location = location.to_str().unwrap_or_default().trim().to_string();
            if location.starts_with("http") {
                return resolve_host(client, &location, depth + 1);
            }
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if content_type.contains("application/octet-stream")
            || content_type.contains("application/zip")
            || content_type.contains("application/x-7z")
            || content_type.contains("application/x-rar")
        {
            return Ok(Resolution::Direct(DirectLink {
                url: url.to_string(),
                headers: vec![("Referer".into(), SITE.into())],
            }));
        }
        if content_type.contains("text/html") {
            let text = response.text().unwrap_or_default();
            if let Some(href) = find_direct_href(&text) {
                let next = absolutize(url, &href);
                if next != url {
                    return resolve_host(client, &next, depth + 1);
                }
            }
        }
    }

    Ok(Resolution::Manual {
        url: url.to_string(),
        reason: "This file host isn't supported automatically. Opening it in your browser.".into(),
    })
}

fn resolve_mediafire(client: &Client, url: &str) -> Result<Resolution, String> {
    let response = client
        .get(url)
        .header(REFERER, SITE)
        .send()
        .map_err(|e| format!("MediaFire error: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("MediaFire returned HTTP {}", response.status()));
    }
    let text = response
        .text()
        .map_err(|e| format!("Could not read MediaFire page: {e}"))?;
    let doc = Html::parse_document(&text);
    if let Some(button) = doc.select(&sel("a#downloadButton")).next() {
        if let Some(href) = attr_of(&button, "href") {
            if href.starts_with("http") {
                return Ok(Resolution::Direct(DirectLink {
                    url: href,
                    headers: vec![("Referer".into(), "https://www.mediafire.com/".into())],
                }));
            }
        }
    }
    // Fallback for the slightly different "download link broken" layout.
    if let Some(href) = doc
        .select(&sel("a[href*=\"download\"]"))
        .filter_map(|a| attr_of(&a, "href"))
        .find(|h| h.contains("mediafire.com/file/") || h.contains("download.mediafire.com"))
    {
        return Ok(Resolution::Direct(DirectLink {
            url: href,
            headers: vec![("Referer".into(), "https://www.mediafire.com/".into())],
        }));
    }
    Err("MediaFire is showing a repair or unavailable page for this file.".into())
}

fn resolve_google_drive(client: &Client, url: &str) -> Result<Resolution, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    let segments = parsed
        .path_segments()
        .map(|s| s.collect::<Vec<_>>())
        .unwrap_or_default();
    let mut id = None;
    if let Some(pos) = segments.iter().position(|s| *s == "d") {
        id = segments.get(pos + 1).map(|s| s.to_string());
    }
    if id.is_none() {
        id = parsed
            .query_pairs()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.to_string());
    }
    let id = id.ok_or_else(|| "Could not find the Google Drive file ID.".to_string())?;

    let mut confirm_url = format!("https://drive.google.com/uc?export=download&id={id}");
    let response = client
        .get(&confirm_url)
        .header(REFERER, "https://drive.google.com/")
        .send()
        .map_err(|e| format!("Google Drive error: {e}"))?;

    let mut cookies = collect_cookies(&response);
    if let Some(location) = response.headers().get(LOCATION) {
        let location = location.to_str().unwrap_or_default().trim().to_string();
        if location.starts_with("http") {
            return Ok(Resolution::Direct(DirectLink {
                url: location,
                headers: vec![("Referer".into(), "https://drive.google.com/".into())],
            }));
        }
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !content_type.contains("text/html") {
        return Ok(Resolution::Direct(DirectLink {
            url: confirm_url,
            headers: vec![("Referer".into(), "https://drive.google.com/".into())],
        }));
    }

    let text = response
        .text()
        .map_err(|e| format!("Could not read Google Drive page: {e}"))?;
    let doc = Html::parse_document(&text);
    if let Some(form) = doc.select(&sel("form#download-form")).next() {
        let action = attr_of(&form, "action").unwrap_or_else(|| confirm_url.clone());
        let mut target = reqwest::Url::parse(&absolutize("https://drive.google.com/", &action))
            .map_err(|e| e.to_string())?;
        {
            let mut query = target.query_pairs_mut();
            for input in form.select(&sel("input[name]")) {
                if let (Some(name), Some(value)) =
                    (attr_of(&input, "name"), attr_of(&input, "value"))
                {
                    query.append_pair(&name, &value);
                }
            }
        }
        confirm_url = target.to_string();
    } else if let Some(link) = doc
        .select(&sel("a[href*=\"confirm\"]"))
        .filter_map(|a| attr_of(&a, "href"))
        .next()
    {
        confirm_url = absolutize("https://drive.google.com/", &link);
    } else {
        return Ok(Resolution::Manual {
            url: url.to_string(),
            reason: "Google Drive wants confirmation that can't be automated. Opening the page in your browser."
                .into(),
        });
    }

    // Ask once more without reading the body, so the final link can be saved and resumed.
    match client
        .get(&confirm_url)
        .header(REFERER, "https://drive.google.com/")
        .send()
    {
        Ok(probe) => {
            cookies.extend(collect_cookies(&probe));
            cookies.sort();
            cookies.dedup();
            if let Some(location) = probe.headers().get(LOCATION) {
                if let Ok(location) = location.to_str() {
                    if location.starts_with("http") {
                        let mut headers =
                            vec![("Referer".into(), "https://drive.google.com/".into())];
                        if !cookies.is_empty() {
                            headers.push(("Cookie".into(), cookies.join("; ")));
                        }
                        return Ok(Resolution::Direct(DirectLink {
                            url: location.to_string(),
                            headers,
                        }));
                    }
                }
            }
        }
        Err(_) => {}
    }

    let mut headers = vec![("Referer".into(), "https://drive.google.com/".into())];
    if !cookies.is_empty() {
        headers.push(("Cookie".into(), cookies.join("; ")));
    }

    Ok(Resolution::Direct(DirectLink {
        url: confirm_url,
        headers,
    }))
}

fn collect_cookies(response: &Response) -> Vec<String> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn looks_like_file(url: &reqwest::Url) -> bool {
    let path = percent_decode_str(url.path()).decode_utf8_lossy();
    let lower = path.to_ascii_lowercase();
    [
        ".zip", ".rar", ".7z", ".tar", ".gz", ".flac", ".mp3", ".ogg", ".opus", ".tta", ".wav",
        ".m4a", ".iso", ".bin",
    ]
    .iter()
    .any(|ext| lower.ends_with(ext))
}

fn find_direct_href(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let anchors = sel("a[href]");
    let mut best: Option<String> = None;
    for anchor in doc.select(&anchors) {
        let href = match attr_of(&anchor, "href") {
            Some(href) => href,
            None => continue,
        };
        if href.starts_with('#') || href.starts_with("javascript") {
            continue;
        }
        let lower = href.to_ascii_lowercase();
        let is_file = [
            ".zip", ".rar", ".7z", ".tar", ".gz", ".flac", ".mp3", ".ogg", ".opus", ".tta",
        ]
        .iter()
        .any(|ext| lower.contains(ext));
        let is_host = lower.contains("mediafire.com")
            || lower.contains("pixeldrain.com")
            || lower.contains("dropbox.com")
            || lower.contains("drive.google.com");
        let is_download_button =
            lower.contains("downloadbutton") || lower.contains("download-link");
        if is_file || is_host || is_download_button {
            best = Some(href);
            if is_file {
                break;
            }
        }
    }
    best
}

fn absolutize(base: &str, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Ok(base) = reqwest::Url::parse(base) {
        if let Ok(url) = base.join(href) {
            return url.to_string();
        }
    }
    href.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_client() -> Client {
        page_client()
    }

    #[test]
    #[ignore = "hits the live DoujinStyle site"]
    fn live_latest_page() {
        let page = fetch_latest(&live_client(), 0).expect("latest feed");
        println!(
            "latest albums: {} (total {:?})",
            page.albums.len(),
            page.total_results
        );
        for album in page.albums.iter().take(5) {
            println!(
                "  {} | {} | {} | new={}",
                album.id, album.title, album.artist, album.is_new
            );
        }
        assert!(!page.albums.is_empty(), "latest feed had no cards");
        assert!(page.albums.iter().all(|a| a.id > 0));
        assert!(page.albums.iter().any(|a| !a.title.is_empty()));
    }

    #[test]
    #[ignore = "hits the live DoujinStyle site"]
    fn live_search() {
        let params = SearchParams {
            text: "rock".into(),
            kind: SearchKind::Tags,
            formats: FormatFilter::default(),
            page: 0,
        };
        let page = fetch_search(&live_client(), &params).expect("search");
        println!(
            "search results: {} (total {:?})",
            page.albums.len(),
            page.total_results
        );
        for album in page.albums.iter().take(5) {
            println!("  {} | {} | {:?}", album.id, album.title, album.tags);
        }
        assert!(!page.albums.is_empty(), "tag search had no cards");
    }

    #[test]
    #[ignore = "hits the live DoujinStyle site"]
    fn live_detail() {
        let client = live_client();
        let feed = fetch_latest(&client, 0).expect("latest feed");
        let album = feed.albums.first().expect("at least one album").clone();
        let detail = fetch_album(&client, &album).expect("album detail");
        println!(
            "detail: {} | artists {:?} | format {} | date {} | tags {:?} | rating {}",
            detail.album.title,
            detail.artists,
            detail.format,
            detail.date_added,
            detail.tags,
            detail.rating
        );
        assert!(!detail.album.title.is_empty());
    }

    #[test]
    #[ignore = "hits the live DoujinStyle site and resolves external hosts"]
    fn live_resolve_first_album() {
        let client = live_client();
        let feed = fetch_latest(&client, 0).expect("latest feed");
        let album = feed.albums.first().expect("at least one album").clone();
        match resolve_download(album.id) {
            Ok(Resolution::Direct(link)) => {
                println!("direct: {} headers {:?}", link.url, link.headers);
                assert!(link.url.starts_with("http"));
            }
            Ok(Resolution::Manual { url, reason }) => {
                println!("manual: {url} ({reason})");
                assert!(url.starts_with("http"));
            }
            Err(error) => panic!("could not resolve album {}: {error}", album.id),
        }
    }
}
