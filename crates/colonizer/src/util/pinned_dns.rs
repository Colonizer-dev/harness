//! Owner-allowlisted private hosts and the pinned DNS resolver behind webhook and subscription
//! deliveries: an address checked as public must stay public when the connection is made (no
//! DNS rebinding to loopback or a private network).

use super::*;

/// Owner-set private hosts allowed through the resolver, e.g. "owner-box.example". Only owner-created
/// webhooks and subscriptions without scoped tokens may be added here. Scoped-token deliveries never
/// bypass the policy.
static ALLOWED_PRIVATE_HOSTS: std::sync::LazyLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashSet::new()));

/// Adds a private host to the allowlist, enabling owner-created subscriptions or webhooks pointing
/// there. Scoped-token subscriptions are never allowlisted.
pub fn allow_private_host(host: &str) {
    if !host.is_empty() {
        ALLOWED_PRIVATE_HOSTS
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host.to_ascii_lowercase());
    }
}

/// A DNS resolver that pins resolved addresses, refusing private addresses unless the host is
/// allowlisted. This prevents DNS rebinding attacks where a name answers with a public address
/// at check time but rebinds to a private address when the connection is made.
struct PinnedResolver;

impl reqwest::dns::Resolve for PinnedResolver {
    fn resolve(
        &self,
        name: reqwest::dns::Name,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<dyn Iterator<Item = std::net::SocketAddr> + Send + 'static>,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                > + Send
                + 'static,
        >,
    > {
        Box::pin(async move {
            // Convert Name to lowercase string for matching against allowlist and test resolver
            let host = name.as_str().to_ascii_lowercase();

            let addrs = {
                #[cfg(test)]
                {
                    if let Some(resolved) = RESOLVER.with_borrow(|stack| stack.last().map(|resolve| resolve(&host))) {
                        if let Some(ips) = resolved {
                            ips.into_iter().map(|ip| (ip, 0).into()).collect::<Vec<_>>()
                        } else {
                            return Err("name does not resolve".into());
                        }
                    } else {
                        Self::system_resolve(&host).await?
                    }
                }
                #[cfg(not(test))]
                {
                    Self::system_resolve(&host).await?
                }
            };

            // Check if all addresses are public, or if the host is allowlisted
            let all_public = addrs.iter().all(|addr| !private_ip(addr.ip()));
            let is_allowlisted = ALLOWED_PRIVATE_HOSTS
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(host.as_str());

            if all_public || is_allowlisted {
                tracing::debug!(host = %host, addrs = ?addrs, "pinned resolver: allowing addresses");
                Ok(Box::new(addrs.into_iter()) as Box<dyn Iterator<Item = std::net::SocketAddr> + Send>)
            } else {
                let ip_list = addrs.iter().map(|a| a.ip().to_string()).collect::<Vec<_>>().join(", ");
                let msg = format!("{host} resolves to a private address ({ip_list}); refused");
                tracing::warn!(host = %host, addrs = %ip_list, "pinned resolver: rejecting private address");
                Err(msg.into())
            }
        })
    }
}

impl PinnedResolver {
    async fn system_resolve(host: &str) -> Result<Vec<std::net::SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
        match tokio::net::lookup_host((host, 0)).await {
            Ok(addrs) => {
                let saddrs: Vec<_> = addrs.collect();
                if saddrs.is_empty() {
                    Err("no addresses found".into())
                } else {
                    Ok(saddrs)
                }
            }
            Err(e) => Err(format!("DNS lookup failed: {e}").into()),
        }
    }
}

/// Creates a DNS resolver that pins addresses to prevent rebinding attacks.
pub fn build_pinned_resolver() -> impl reqwest::dns::Resolve {
    PinnedResolver
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DNS rebinding attack: first query answers with a public address to pass the policy check,
    /// but the second query (when the connection is made) answers with loopback to redirect there.
    /// The pinned resolver refuses this by pinning the first answer.
    #[tokio::test]
    async fn pinned_resolver_refuses_rebinding_redirects() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static QUERY_COUNT: AtomicUsize = AtomicUsize::new(0);

        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "rebind.example" => {
                    // First query returns public, second+ returns loopback
                    let count = QUERY_COUNT.fetch_add(1, Ordering::SeqCst);
                    if count == 0 {
                        Some(vec![ip("93.184.216.34")]) // public on first query
                    } else {
                        Some(vec![ip("127.0.0.1")]) // loopback on rebind
                    }
                }
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Server A: accepts one connection and 302-redirects to rebind.example
        let server_a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_a_addr = server_a.local_addr().unwrap().to_string();

        // Server B: counts connections (should be zero if resolver is pinning)
        let server_b = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_b_addr = server_b.local_addr().unwrap().port();
        let connection_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let connection_count_clone = connection_count.clone();

        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = server_a.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                // Redirect to rebind.example on server B's port
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://rebind.example:{}/\r\n\
                    Content-Length: 0\r\nConnection: close\r\n\r\n",
                    server_b_addr
                );
                let _ = conn.write_all(resp.as_bytes());
            }
        });

        std::thread::spawn(move || {
            // Server B: count any connections (there should be none because the resolver refuses)
            server_b.set_nonblocking(true).ok();
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            loop {
                match server_b.accept() {
                    Ok(_) => {
                        connection_count_clone.store(1, Ordering::SeqCst);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() > deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        // Client with pinned resolver should reject the rebinding
        let client = reqwest::Client::builder()
            .redirect(public_redirect_policy())
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        let err = client
            .get(format!("http://{server_a_addr}/"))
            .send()
            .await
            .expect_err("the pinned resolver should refuse the rebinding");

        // Verify the refusal is in the error chain
        let mut sees_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            sees_refusal |= e.to_string().contains("private");
            layer = e.source();
        }
        assert!(sees_refusal, "error should mention private address refusal: {err}");

        // Verify server B was never reached
        assert_eq!(
            connection_count.load(Ordering::SeqCst),
            0,
            "the loopback server should never be dialed"
        );
    }

    #[tokio::test]
    async fn pinned_resolver_refuses_private_addresses() {
        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "rebind.example" => Some(vec![ip("127.0.0.1")]),
                "public.example" => Some(vec![ip("93.184.216.34")]),
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Build a client with the pinned resolver
        let client = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        // A request to a public address should work (or at least not fail on DNS resolution)
        // A request to a private address should fail
        let err = client
            .get("http://rebind.example/test")
            .send()
            .await
            .expect_err("the resolver should refuse the private address");

        // The error should mention the refusal
        let mut mentions_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            mentions_refusal |= e.to_string().contains("private") || e.to_string().contains("refused");
            layer = e.source();
        }
        assert!(mentions_refusal, "error should mention the refusal: {err}");
    }

    #[tokio::test]
    async fn pinned_resolver_allows_allowlisted_private_hosts() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "owner-box.example" => Some(vec![ip("127.0.0.1")]),
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Before allowing, the resolver should reject
        let client1 = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        let err = client1
            .get(format!("http://owner-box.example:{port}/test"))
            .send()
            .await
            .expect_err("private address should be refused before allowlisting");

        let mut mentions_refusal = false;
        let mut layer = Some(&err as &dyn std::error::Error);
        while let Some(e) = layer {
            mentions_refusal |= e.to_string().contains("private") || e.to_string().contains("refused");
            layer = e.source();
        }
        assert!(mentions_refusal, "should refuse private address initially");

        // After allowlisting, the resolver should allow it
        allow_private_host("owner-box.example");

        // Set up a simple server that responds with 200
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK");
            }
        });

        let client2 = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .build()
            .unwrap();

        // Now it should reach the address after allowlisting
        let result = client2.get(format!("http://owner-box.example:{port}/test")).send().await;

        // Either it succeeds, or it fails with a connection error (not a DNS refusal)
        match result {
            Ok(_) => {} // Success
            Err(e) => {
                // Make sure it's not a DNS refusal
                let mut mentions_refusal = false;
                let mut layer = Some(&e as &dyn std::error::Error);
                while let Some(err) = layer {
                    mentions_refusal |= err.to_string().contains("private") || err.to_string().contains("refused");
                    layer = err.source();
                }
                assert!(!mentions_refusal, "after allowlisting, should not refuse: {e}");
            }
        }
    }

    /// Allowlisted private host delivers end-to-end: an allowlisted hostname resolving to 127.0.0.1
    /// can deliver through a client with the pinned resolver.
    #[tokio::test]
    async fn allowlisted_private_webhook_delivers_end_to_end() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        fn resolve(host: &str) -> Option<Vec<IpAddr>> {
            let ip = |s: &str| s.parse::<IpAddr>().unwrap();
            match host {
                "owner-private.example" => Some(vec![ip("127.0.0.1")]),
                _ => None,
            }
        }
        let _guard = install_test_resolver(resolve);

        // Set up a loopback server that accepts one connection and responds with 200
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");
            }
        });

        // Allowlist the hostname
        allow_private_host("owner-private.example");

        // Build a client like the delivery clients: pinned resolver + redirect policy
        let client = reqwest::Client::builder()
            .dns_resolver(build_pinned_resolver())
            .redirect(public_redirect_policy())
            .build()
            .unwrap();

        // Request to the allowlisted private hostname succeeds
        let resp = client
            .get(format!("http://owner-private.example:{port}/test"))
            .send()
            .await
            .expect("allowlisted private host should deliver");

        assert_eq!(resp.status(), 200);
        let body = resp.text().await.expect("read body");
        assert_eq!(body, "hello");
    }
}
