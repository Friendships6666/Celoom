//! Public IP intelligence, BGP topology, RPKI, DNSBL, and multi-source GeoIP enrichment
//! for network diagnostic probes. All external queries are strictly bounded and optional.
use reqwest::Client;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};
use tokio::net::{TcpStream, lookup_host};
use tokio_util::sync::CancellationToken;

const TIER1_ASNS: &[u64] = &[
    174, 209, 286, 701, 1239, 1299, 2914, 3257, 3320, 3356, 3491, 5511, 6453, 6461, 6762, 6830,
    7018, 12956,
];

const COMMON_PORTS: &[(u16, &str)] = &[
    (80, "HTTP"),
    (443, "HTTPS"),
    (22, "SSH"),
    (8080, "HTTP-Alt"),
    (8443, "HTTPS-Alt"),
];

const DNSBL_ZONES: &[&str] = &[
    "zen.spamhaus.org",
    "bl.spamcop.net",
    "b.barracudacentral.org",
    "dnsbl.sorbs.net",
];

pub fn is_bogon(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            let cgnat = o[0] == 100 && (o[1] & 0b1100_0000) == 64;
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || cgnat
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            let doc = s[0] == 0x2001 && s[1] == 0x0db8;
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6.is_unique_local()
                || doc
        }
    }
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6371.0_f64;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().atan2((1.0 - a).sqrt())
}

async fn lookup_ptr(ip: IpAddr) -> Option<String> {
    let ip_str = ip.to_string();
    let fut = tokio::process::Command::new("dig")
        .args(["-x", &ip_str, "+short", "+time=1", "+tries=1"])
        .output();
    if let Ok(Ok(out)) = tokio::time::timeout(Duration::from_millis(1600), fut).await
        && out.status.success()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(line) = text.lines().find(|l| !l.trim().is_empty()) {
            return Some(line.trim().trim_end_matches('.').to_string());
        }
    }
    None
}

async fn probe_ports(ip: IpAddr) -> Vec<Value> {
    let mut handles = Vec::with_capacity(COMMON_PORTS.len());
    for &(port, service) in COMMON_PORTS {
        handles.push(tokio::spawn(async move {
            let open = tokio::time::timeout(
                Duration::from_millis(650),
                TcpStream::connect((ip, port)),
            )
            .await
            .is_ok_and(|res| res.is_ok());
            json!({ "port": port, "service": service, "open": open })
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        if let Ok(v) = h.await {
            out.push(v);
        }
    }
    out
}

async fn probe_dnsbl(ip: IpAddr) -> Value {
    let IpAddr::V4(v4) = ip else {
        return json!({
            "checked": false,
            "listed": false,
            "providers_checked": 0,
            "hits": [],
            "note": "IPv6 地址暂不查询 DNSBL 黑名单"
        });
    };
    let [a, b, c, d] = v4.octets();
    let rev = format!("{d}.{c}.{b}.{a}");
    let mut handles = Vec::with_capacity(DNSBL_ZONES.len());
    for &zone in DNSBL_ZONES {
        let host = format!("{rev}.{zone}:80");
        handles.push(tokio::spawn(async move {
            let res = tokio::time::timeout(Duration::from_millis(1400), lookup_host(&host)).await;
            let hit = match res {
                Ok(Ok(addrs)) => addrs.into_iter().any(|sa| match sa.ip() {
                    // Spamhaus returns 127.255.255.* for query-policy errors, not listings.
                    IpAddr::V4(addr) => {
                        let o = addr.octets();
                        o[0] == 127 && o[1] == 0 && o[2] == 0
                    }
                    _ => false,
                }),
                _ => false,
            };
            (zone, hit)
        }));
    }
    let mut hits = Vec::new();
    for h in handles {
        if let Ok((zone, true)) = h.await {
            hits.push(zone.to_string());
        }
    }
    json!({
        "checked": true,
        "listed": !hits.is_empty(),
        "providers_checked": DNSBL_ZONES.len(),
        "hits": hits,
    })
}

async fn fetch_json(client: &Client, url: &str, cancel: &CancellationToken) -> Option<Value> {
    if cancel.is_cancelled() {
        return None;
    }
    let req = client.get(url).send();
    let resp = tokio::select! {
        _ = cancel.cancelled() => return None,
        res = tokio::time::timeout(Duration::from_millis(3200), req) => res.ok()?.ok()?,
    };
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

fn parse_asn_num(raw: &str) -> Option<u64> {
    raw.trim()
        .trim_start_matches("AS")
        .trim_start_matches("as")
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
}

async fn fetch_as_name(client: &Client, asn: u64, cancel: &CancellationToken) -> Option<String> {
    let url = format!("https://stat.ripe.net/data/as-overview/data.json?resource=AS{asn}");
    let json = fetch_json(client, &url, cancel).await?;
    json["data"]["holder"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_bgp(client: &Client, ip: IpAddr, cancel: &CancellationToken) -> Value {
    let info_url = format!("https://stat.ripe.net/data/network-info/data.json?resource={ip}");
    let Some(info) = fetch_json(client, &info_url, cancel).await else {
        return Value::Null;
    };
    let prefix = info["data"]["prefix"].as_str().map(str::to_string);
    let asn = info["data"]["asns"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.as_str().and_then(parse_asn_num).or_else(|| v.as_u64()));

    let Some(asn) = asn else {
        return json!({ "prefix": prefix });
    };

    let overview_url = format!("https://stat.ripe.net/data/as-overview/data.json?resource=AS{asn}");
    let neighbours_url =
        format!("https://stat.ripe.net/data/asn-neighbours/data.json?resource=AS{asn}");
    let prefixes_url =
        format!("https://stat.ripe.net/data/announced-prefixes/data.json?resource=AS{asn}");
    let rpki_url = prefix.as_ref().map(|p| {
        format!("https://stat.ripe.net/data/rpki-validation/data.json?resource={asn}&prefix={p}")
    });

    let (overview, neighbours, prefixes, rpki) = tokio::join!(
        fetch_json(client, &overview_url, cancel),
        fetch_json(client, &neighbours_url, cancel),
        fetch_json(client, &prefixes_url, cancel),
        async {
            if let Some(u) = &rpki_url {
                fetch_json(client, u, cancel).await
            } else {
                None
            }
        }
    );

    let as_name = overview
        .as_ref()
        .and_then(|v| v["data"]["holder"].as_str())
        .map(|s| s.trim().to_string());

    let rpki_status = rpki
        .as_ref()
        .and_then(|v| v["data"]["status"].as_str())
        .map(str::to_string);

    let mut upstreams_raw: Vec<(u64, u64)> = Vec::new();
    let mut downstreams_raw: Vec<(u64, u64)> = Vec::new();
    let mut total_left_power = 0u64;
    let mut observed_paths = 0u64;

    if let Some(list) = neighbours
        .as_ref()
        .and_then(|v| v["data"]["neighbours"].as_array())
    {
        for item in list {
            let Some(n_asn) = item["asn"].as_u64() else {
                continue;
            };
            let power = item["power"].as_u64().unwrap_or(1);
            observed_paths += power;
            match item["type"].as_str() {
                Some("left") => {
                    total_left_power += power;
                    upstreams_raw.push((n_asn, power));
                }
                Some("right") => downstreams_raw.push((n_asn, power)),
                _ => {}
            }
        }
    }
    upstreams_raw.sort_by(|a, b| b.1.cmp(&a.1));
    upstreams_raw.truncate(5);
    downstreams_raw.sort_by(|a, b| b.1.cmp(&a.1));
    downstreams_raw.truncate(12);

    let mut name_targets: Vec<u64> = upstreams_raw
        .iter()
        .map(|x| x.0)
        .chain(downstreams_raw.iter().take(8).map(|x| x.0))
        .collect();
    name_targets.sort_unstable();
    name_targets.dedup();

    let name_futs = name_targets
        .iter()
        .map(|&n| async move { (n, fetch_as_name(client, n, cancel).await) });
    let resolved_names: std::collections::HashMap<u64, String> =
        futures_util::future::join_all(name_futs)
            .await
            .into_iter()
            .filter_map(|(n, opt)| opt.map(|name| (n, name)))
            .collect();

    let upstreams: Vec<Value> = upstreams_raw
        .into_iter()
        .map(|(n_asn, power)| {
            let share_pct = if total_left_power > 0 {
                ((power as f64 / total_left_power as f64) * 1000.0).round() / 10.0
            } else {
                0.0
            };
            json!({
                "asn": n_asn,
                "name": resolved_names.get(&n_asn).cloned(),
                "power": power,
                "share_pct": share_pct,
                "is_tier1": TIER1_ASNS.contains(&n_asn),
            })
        })
        .collect();

    let downstreams: Vec<Value> = downstreams_raw
        .into_iter()
        .map(|(n_asn, power)| {
            json!({
                "asn": n_asn,
                "name": resolved_names.get(&n_asn).cloned(),
                "power": power,
            })
        })
        .collect();

    let announced_prefixes: Vec<String> = prefixes
        .as_ref()
        .and_then(|v| v["data"]["prefixes"].as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p["prefix"].as_str().map(str::to_string))
                .take(12)
                .collect()
        })
        .unwrap_or_default();

    json!({
        "asn": asn,
        "as_name": as_name,
        "prefix": prefix,
        "rpki_status": rpki_status,
        "observed_paths": observed_paths,
        "upstreams": upstreams,
        "downstreams": downstreams,
        "announced_prefixes": announced_prefixes,
    })
}

pub async fn build_ip_report(
    ip: IpAddr,
    target_host: Option<&str>,
    tls_subject: Option<&str>,
    cancel: &CancellationToken,
) -> Value {
    let version = if ip.is_ipv4() { "IPv4" } else { "IPv6" };
    if is_bogon(ip) {
        return json!({
            "ip": ip.to_string(),
            "version": version,
            "bogon": true,
            "notes": ["内网 / 保留地址（Bogon），跳过公网 IP 情报与 BGP 拓扑查询。"],
        });
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent("Celoom-NetworkDiagnostic/1.0")
        .build()
        .unwrap_or_else(|_| Client::new());

    let ip_api_url = format!(
        "http://ip-api.com/json/{ip}?fields=status,country,countryCode,regionName,city,lat,lon,isp,org,as,asname,reverse,mobile,proxy,hosting,timezone"
    );
    let ipwho_url = format!("https://ipwho.is/{ip}");
    let ipsb_url = format!("https://api.ip.sb/geoip/{ip}");

    let (ptr, ports, dnsbl, ip_api, ipwho, ipsb, bgp) = tokio::join!(
        lookup_ptr(ip),
        probe_ports(ip),
        probe_dnsbl(ip),
        fetch_json(&client, &ip_api_url, cancel),
        fetch_json(&client, &ipwho_url, cancel),
        fetch_json(&client, &ipsb_url, cancel),
        fetch_bgp(&client, ip, cancel),
    );

    let mut geo_sources = Vec::new();
    let mut coords: Vec<(f64, f64)> = Vec::new();
    let mut reverse_dns = ptr;
    let mut proxy_flag: Option<bool> = None;
    let mut hosting_flag: Option<bool> = None;
    let mut mobile_flag: Option<bool> = None;
    let mut isp: Option<String> = None;
    let mut org: Option<String> = None;

    if let Some(v) = ip_api.as_ref().filter(|v| v["status"] == "success") {
        let lat = v["lat"].as_f64();
        let lon = v["lon"].as_f64();
        if let (Some(la), Some(lo)) = (lat, lon) {
            coords.push((la, lo));
        }
        if reverse_dns.is_none() {
            reverse_dns = v["reverse"]
                .as_str()
                .map(|s| s.trim().trim_end_matches('.').to_string())
                .filter(|s| !s.is_empty());
        }
        proxy_flag = v["proxy"].as_bool();
        hosting_flag = v["hosting"].as_bool();
        mobile_flag = v["mobile"].as_bool();
        isp = v["isp"].as_str().map(str::to_string).filter(|s| !s.is_empty());
        org = v["org"].as_str().map(str::to_string).filter(|s| !s.is_empty());
        geo_sources.push(json!({
            "source": "ip-api",
            "country": v["country"].as_str(),
            "country_code": v["countryCode"].as_str(),
            "region": v["regionName"].as_str(),
            "city": v["city"].as_str(),
            "lat": lat,
            "lon": lon,
            "timezone": v["timezone"].as_str(),
            "isp": v["isp"].as_str(),
            "org": v["org"].as_str(),
        }));
    }

    if let Some(v) = ipwho.as_ref().filter(|v| v["success"] == true) {
        let lat = v["latitude"].as_f64();
        let lon = v["longitude"].as_f64();
        if let (Some(la), Some(lo)) = (lat, lon) {
            coords.push((la, lo));
        }
        if isp.is_none() {
            isp = v["connection"]["isp"].as_str().map(str::to_string);
        }
        if org.is_none() {
            org = v["connection"]["org"].as_str().map(str::to_string);
        }
        geo_sources.push(json!({
            "source": "ipwho.is",
            "country": v["country"].as_str(),
            "country_code": v["country_code"].as_str(),
            "region": v["region"].as_str(),
            "city": v["city"].as_str(),
            "lat": lat,
            "lon": lon,
            "timezone": v["timezone"]["id"].as_str(),
            "isp": v["connection"]["isp"].as_str(),
            "org": v["connection"]["org"].as_str(),
        }));
    }

    if let Some(v) = ipsb.as_ref().filter(|v| v["country"].is_string()) {
        let lat = v["latitude"].as_f64();
        let lon = v["longitude"].as_f64();
        if let (Some(la), Some(lo)) = (lat, lon) {
            coords.push((la, lo));
        }
        geo_sources.push(json!({
            "source": "ip.sb",
            "country": v["country"].as_str(),
            "country_code": v["country_code"].as_str(),
            "region": v["region"].as_str(),
            "city": v["city"].as_str(),
            "lat": lat,
            "lon": lon,
            "timezone": v["timezone"].as_str(),
            "isp": v["isp"].as_str(),
            "org": v["asn_organization"].as_str(),
        }));
    }

    let mut max_offset_km = 0.0_f64;
    for i in 0..coords.len() {
        for j in (i + 1)..coords.len() {
            let d = haversine_km(coords[i].0, coords[i].1, coords[j].0, coords[j].1);
            if d > max_offset_km {
                max_offset_km = d;
            }
        }
    }
    let max_offset_km = if coords.len() >= 2 {
        Some(max_offset_km.round() as u64)
    } else {
        None
    };

    let dnsbl_listed = dnsbl["listed"].as_bool() == Some(true);
    let risk_level = if dnsbl_listed {
        "发现黑名单威胁"
    } else if proxy_flag == Some(true) {
        "代理 / 中转节点"
    } else if !geo_sources.is_empty() || dnsbl["checked"].as_bool() == Some(true) {
        "未发现明显威胁"
    } else {
        "未知"
    };
    let abuse_level = if dnsbl_listed {
        "存在滥用记录"
    } else if dnsbl["checked"].as_bool() == Some(true) {
        "极度纯净"
    } else if proxy_flag == Some(false) {
        "正常"
    } else {
        "未知"
    };

    let mut domains = HashSet::new();
    if let Some(h) = target_host
        && h.parse::<IpAddr>().is_err()
        && !h.is_empty()
    {
        domains.insert(h.to_string());
    }
    if let Some(r) = &reverse_dns
        && !r.is_empty()
    {
        domains.insert(r.clone());
    }
    if let Some(sub) = tls_subject {
        for part in sub.split(',') {
            let trimmed = part.trim();
            if let Some(cn) = trimmed.strip_prefix("CN=").or_else(|| trimmed.strip_prefix("CN = ")) {
                let val = cn.trim();
                if !val.is_empty() && val.contains('.') {
                    domains.insert(val.to_string());
                }
            }
        }
    }
    let mut associated_domains: Vec<String> = domains.into_iter().collect();
    associated_domains.sort();

    // Fallback ASN from ip-api if RIPEstat failed
    let mut bgp_out = bgp;
    if bgp_out.is_null()
        && let Some(as_str) = ip_api
            .as_ref()
            .and_then(|v| v["as"].as_str())
        && let Some(asn) = parse_asn_num(as_str)
    {
        bgp_out = json!({
            "asn": asn,
            "as_name": ip_api.as_ref().and_then(|v| v["asname"].as_str()).or(org.as_deref()),
        });
    }

    json!({
        "ip": ip.to_string(),
        "version": version,
        "bogon": false,
        "reverse_dns": reverse_dns,
        "isp": isp,
        "org": org,
        "open_ports": ports,
        "dnsbl": dnsbl,
        "security": {
            "risk_level": risk_level,
            "abuse_level": abuse_level,
            "proxy": proxy_flag,
            "hosting": hosting_flag,
            "mobile": mobile_flag,
            "rpki_status": bgp_out["rpki_status"].as_str(),
        },
        "geo": {
            "primary": geo_sources.first().cloned().unwrap_or(Value::Null),
            "sources": geo_sources,
            "max_offset_km": max_offset_km,
        },
        "bgp": bgp_out,
        "associated_domains": associated_domains,
    })
}

pub fn check_ipv4_bogon_for_test(ip: Ipv4Addr) -> bool {
    is_bogon(IpAddr::V4(ip))
}

fn extract_html_title(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let start = lower.find("<title>")?;
    let rest = &body[start + 7..];
    let end = rest.to_ascii_lowercase().find("</title>")?;
    let title = rest[..end].split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(80).collect())
    }
}

fn header_str(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().chars().take(128).collect())
        .filter(|s: &String| !s.is_empty())
}

async fn probe_one_endpoint(
    client: &Client,
    url: &str,
    cancel: &CancellationToken,
) -> Option<(u16, reqwest::header::HeaderMap, String)> {
    if cancel.is_cancelled() {
        return None;
    }
    let req = client.get(url).send();
    let resp = tokio::select! {
        _ = cancel.cancelled() => return None,
        res = tokio::time::timeout(Duration::from_millis(2600), req) => res.ok()?.ok()?,
    };
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let text = tokio::time::timeout(Duration::from_millis(1200), resp.text())
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();
    let snippet: String = text.chars().take(8192).collect();
    Some((status, headers, snippet))
}

/// Anonymous probe of common relay/gateway public endpoints (`/api/status`, `/v1/models`, `/`, `/health`)
/// without sending any Authorization or Cookie headers.
pub async fn probe_gateway(origin: &str, cancel: &CancellationToken) -> Value {
    let origin = origin.trim_end_matches('/');
    let client = Client::builder()
        .timeout(Duration::from_millis(3000))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Celoom-NetworkDiagnostic/1.0")
        .build()
        .unwrap_or_else(|_| Client::new());

    let api_status_url = format!("{origin}/api/status");
    let models_url = format!("{origin}/v1/models");
    let root_url = format!("{origin}/");
    let health_url = format!("{origin}/health");

    let (api_status_res, models_res, root_res, health_res) = tokio::join!(
        probe_one_endpoint(&client, &api_status_url, cancel),
        probe_one_endpoint(&client, &models_url, cancel),
        probe_one_endpoint(&client, &root_url, cancel),
        probe_one_endpoint(&client, &health_url, cancel),
    );

    let mut platform_hints: Vec<String> = Vec::new();
    let mut api_status_info = Value::Null;
    if let Some((status, headers, body)) = api_status_res {
        let parsed: Option<Value> = serde_json::from_str(&body).ok();
        let data = parsed.as_ref().map(|v| &v["data"]);
        let sys_name = data.and_then(|d| d["system_name"].as_str()).map(str::to_string);
        let version = data.and_then(|d| d["version"].as_str()).map(str::to_string);
        let start_time = data.and_then(|d| d["start_time"].as_i64());
        let server = header_str(&headers, "server");
        if parsed.as_ref().and_then(|v| v["success"].as_bool()) == Some(true)
            && (sys_name.is_some() || version.is_some())
        {
            let lower = body.to_ascii_lowercase();
            if lower.contains("new-api") || lower.contains("newapi") {
                platform_hints.push("NewAPI".into());
            } else if lower.contains("veloera") {
                platform_hints.push("Veloera".into());
            } else if lower.contains("done-hub") || lower.contains("donehub") {
                platform_hints.push("DoneHub".into());
            } else {
                platform_hints.push("NewAPI / OneAPI 系列网关".into());
            }
        }
        api_status_info = json!({
            "http_status": status,
            "system_name": sys_name,
            "version": version,
            "start_time": start_time,
            "server": server,
        });
    }

    let mut models_info = Value::Null;
    if let Some((status, headers, body)) = models_res {
        let parsed: Option<Value> = serde_json::from_str(&body).ok();
        let err_type = parsed
            .as_ref()
            .and_then(|v| v["error"]["type"].as_str())
            .map(str::to_string);
        let err_msg = parsed
            .as_ref()
            .and_then(|v| v["error"]["message"].as_str())
            .map(|s| s.chars().take(160).collect::<String>());
        let public_count = parsed
            .as_ref()
            .and_then(|v| v["data"].as_array())
            .map(|a| a.len());
        if let Some(et) = &err_type {
            match et.as_str() {
                "new_api_error" => platform_hints.push("NewAPI (new_api_error)".into()),
                "one_api_error" => platform_hints.push("OneAPI (one_api_error)".into()),
                "rix_api_error" => platform_hints.push("RixAPI (rix_api_error)".into()),
                "veloera_error" => platform_hints.push("Veloera (veloera_error)".into()),
                _ => {}
            }
        }
        if header_str(&headers, "x-oneapi-request-id").is_some() {
            platform_hints.push("OneAPI / NewAPI 请求头 (x-oneapi-request-id)".into());
        }
        if header_str(&headers, "x-litellm-call-id").is_some()
            || header_str(&headers, "x-litellm-response-duration-ms").is_some()
        {
            platform_hints.push("LiteLLM Proxy".into());
        }
        models_info = json!({
            "http_status": status,
            "error_type": err_type,
            "error_message": err_msg,
            "public_models_count": public_count,
            "server": header_str(&headers, "server"),
        });
    }

    let mut root_info = Value::Null;
    if let Some((status, headers, body)) = root_res {
        let title = extract_html_title(&body);
        let server = header_str(&headers, "server");
        let via = header_str(&headers, "via");
        let powered_by = header_str(&headers, "x-powered-by");
        if let Some(t) = &title {
            let tl = t.to_ascii_lowercase();
            if tl.contains("new api") && !platform_hints.iter().any(|p| p.contains("NewAPI")) {
                platform_hints.push("NewAPI (首页标题)".into());
            } else if tl.contains("one api") && !platform_hints.iter().any(|p| p.contains("OneAPI")) {
                platform_hints.push("OneAPI (首页标题)".into());
            }
        }
        root_info = json!({
            "http_status": status,
            "title": title,
            "server": server,
            "via": via,
            "powered_by": powered_by,
        });
    }

    let mut health_info = Value::Null;
    if let Some((status, headers, body)) = health_res {
        let trimmed = body.trim();
        let snippet = if trimmed.len() <= 120 && !trimmed.starts_with('<') {
            Some(trimmed.to_string())
        } else {
            None
        };
        health_info = json!({
            "http_status": status,
            "server": header_str(&headers, "server"),
            "snippet": snippet,
        });
    }

    platform_hints.sort();
    platform_hints.dedup();

    json!({
        "origin": origin,
        "platform_hints": platform_hints,
        "api_status": api_status_info,
        "v1_models": models_info,
        "root": root_info,
        "health": health_info,
    })
}
