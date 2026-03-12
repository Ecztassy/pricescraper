use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use std::collections::HashSet;
use std::io::{self, BufRead};
use std::process::Command;
use std::time::Duration;
use tokio::time::sleep;

fn find_chromium() -> Option<String> {
    let browsers = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "brave-browser",
        "brave",
        "microsoft-edge",
    ];
    for browser in browsers {
        let output = Command::new("which").arg(browser).output().ok()?;
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }
    None
}

/// Converts a URL slug like "iphone-12-128gb-azul-1235801380" into "iphone 12 128gb azul"
fn title_from_url(url: &str) -> String {
    url.split("/item/")
        .nth(1)
        .unwrap_or("")
        .trim_end_matches('/')
        .split('-')
        .filter(|part| {
            // Drop the trailing numeric ID (all-digit segment at the end)
            !part.chars().all(|c| c.is_ascii_digit()) || part.len() < 7
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

async fn dismiss_popups(page: &chromiumoxide::Page) {
    let js = r#"
        (function() {
            const selectors = [
                '[id*="accept"]', '[class*="accept"]',
                '[id*="cookie"] button', '[class*="cookie"] button',
                'button[id*="consent"]', '#onetrust-accept-btn-handler'
            ];
            for (const sel of selectors) {
                const el = document.querySelector(sel);
                if (el) { el.click(); return true; }
            }
            return false;
        })()
    "#;
    let _ = page.evaluate(js).await;
}

async fn scrape_prices(
    page: &chromiumoxide::Page,
    keyword: &str,
    blacklist: &[String],
) -> Vec<(String, f64, String)> {
    let keywords_json = serde_json::to_string(
        &keyword
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();

    let blacklist_json = serde_json::to_string(
        &blacklist
            .iter()
            .map(|s| s.to_lowercase())
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();

    let js = format!(
        r#"
        (function() {{
            const keywords = {keywords_json};
            const blacklist = {blacklist_json};
            const results = [];
            const priceEls = document.querySelectorAll('strong[aria-label="Item price"]');

            priceEls.forEach(el => {{
                const card = el.closest('a[href]');
                const url = card ? card.href : 'unknown';

                // Parse price
                const raw = el.textContent.replace(/\u00a0/g, ' ').trim();
                const numeric = raw.replace(/[^\d,.]/g, '').replace('.', '').replace(',', '.');
                const price = parseFloat(numeric);

                if (!isNaN(price) && url !== 'unknown') {{
                    // Extract slug from URL for filtering — title cleaning done in Rust
                    const slug = url.split('/item/')[1] || '';
                    const slugLower = slug.toLowerCase();

                    const matchesKeyword = keywords.every(kw => slugLower.includes(kw));
                    const isBlacklisted = blacklist.some(word => slugLower.includes(word));

                    if (matchesKeyword && !isBlacklisted) {{
                        results.push({{ price, url }});
                    }} else {{
                        console.log('[filtered] ' + slug + ' => ' + price + '€');
                    }}
                }}
            }});

            return JSON.stringify(results);
        }})()
    "#
    );

    match page.evaluate(js).await {
        Ok(result) => {
            let json_str = result
                .value()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();

            match serde_json::from_str::<Vec<serde_json::Value>>(&json_str) {
                Ok(items) => items
                    .iter()
                    .filter_map(|item| {
                        let price = item["price"].as_f64()?;
                        let url = item["url"].as_str().unwrap_or("unknown").to_string();
                        let title = title_from_url(&url);
                        Some((title, price, url))
                    })
                    .collect(),
                Err(e) => {
                    println!("[scrape] JSON parse error: {}", e);
                    vec![]
                }
            }
        }
        Err(e) => {
            println!("[scrape] JS error: {}", e);
            vec![]
        }
    }
}

async fn click_load_more(page: &chromiumoxide::Page) -> bool {
    for _ in 0..20 {
        let js = r#"
            (function() {
                const btn = document.querySelector('walla-button[text="Cargar más"]');
                if (!btn) return false;

                if (btn.shadowRoot) {
                    const b = btn.shadowRoot.querySelector('button');
                    if (b) { b.click(); return true; }
                }

                btn.click();
                return true;
            })()
        "#;

        if let Ok(res) = page.evaluate(js).await {
            if res.value().and_then(|v| v.as_bool()).unwrap_or(false) {
                return true;
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }

    false
}

fn read_line(prompt: &str) -> String {
    print!("{}", prompt);
    let _ = io::Write::flush(&mut io::stdout());
    let stdin = io::stdin();
    stdin
        .lock()
        .lines()
        .next()
        .unwrap_or(Ok(String::new()))
        .unwrap_or_default()
}

async fn scroll_down(page: &chromiumoxide::Page) {
    let js = r#"
    window.scrollBy(0, window.innerHeight * 20);
    window.dispatchEvent(new Event('scroll'));
    "#;

    let _ = page.evaluate(js).await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let keyword = read_line("Search keyword:\n");
    let keyword = keyword.trim().to_string();

    let limit_str = read_line("Max listings to analyze:\n");
    let limit: usize = limit_str.trim().parse().unwrap_or(50);

    // Blacklist input
    println!("Enter blacklist words (comma-separated), or press Enter to skip:");
    println!("  e.g: funda,cargador,bateria,cable,pack");
    let blacklist_input = read_line("");
    let blacklist: Vec<String> = if blacklist_input.trim().is_empty() {
        vec![]
    } else {
        blacklist_input
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    };

    if blacklist.is_empty() {
        println!("No blacklist words set.");
    } else {
        println!("Blacklist: {:?}", blacklist);
    }

    let browser_path = find_chromium().expect("No Chromium browser found");
    let user_data_dir = "/tmp/wallapop_scraper_profile";
    println!("Using browser: {}", browser_path);

    let (mut browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .chrome_executable(browser_path)
            .args(vec![
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--disable-gpu",
                "--lang=es",
                "--disable-background-networking",
                "--disable-features=RendererCodeIntegrity",
                "--disable-site-isolation-trials",
                "--no-first-run",
                "--incognito",
                "--no-default-browser-check",
                "--blink-settings=imagesEnabled=false",
                &format!("--user-data-dir={}", user_data_dir),
            ])
            .with_head()
            .build()?,
    )
    .await?;

    tokio::spawn(async move { while let Some(_) = handler.next().await {} });

    let page = browser.new_page("about:blank").await?;
    let url = format!("https://es.wallapop.com/app/search?keywords={}", keyword);
    println!("[nav] Going to {}", url);
    page.goto(&url).await?;

    println!("[nav] Waiting for page to load...");
    sleep(Duration::from_millis(60)).await;
    dismiss_popups(&page).await;

    let mut prices: Vec<f64> = Vec::new();
    let mut titles: Vec<String> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    println!("[init] Waiting for 'Cargar más'...");

    if click_load_more(&page).await {
        println!("[init] Infinite scroll activated");
        sleep(Duration::from_millis(200)).await;
    } else {
        println!("[init] No load-more button found");
    }

    loop {
        let found = scrape_prices(&page, &keyword, &blacklist).await;
        println!("[scrape] Got {} matching items from DOM", found.len());

        for (title, price, item_url) in found {
            let key = format!("{}:{}", item_url, price);

            if !seen.contains(&key) {
                seen.insert(key);

                println!("  + '{}' => {:.2}€ ({})", title, price, item_url);

                prices.push(price);
                titles.push(title);
                urls.push(item_url);
            }
        }

        println!("[loop] Unique collected: {}/{}", prices.len(), limit);

        if prices.len() >= limit {
            break;
        }

        println!("[scroll] Scrolling for more listings...");
        scroll_down(&page).await;

        sleep(Duration::from_millis(70)).await;
    }

    browser.close().await?;

    prices.truncate(limit);
    titles.truncate(limit);
    urls.truncate(limit);

    if prices.is_empty() {
        println!("No prices found.");
        return Ok(());
    }

    let sum: f64 = prices.iter().sum();
    let avg = sum / prices.len() as f64;

    println!("\n=== RESULTS ===");
    println!("Listings analyzed: {}", prices.len());
    println!("Average price: {:.2}€", avg);

    println!("\nDo you want to download the results? (y/n)");
    let answer = read_line("");
    if answer.trim().to_lowercase() == "y" {
        let mut wtr = csv::Writer::from_path("average_price.csv")?;
        wtr.write_record(["title", "price", "url"])?;
        for i in 0..prices.len() {
            wtr.write_record(&[
                titles.get(i).unwrap_or(&"unknown".to_string()),
                &prices[i].to_string(),
                urls.get(i).unwrap_or(&"unknown".to_string()),
            ])?;
        }
        wtr.flush()?;
        wtr.write_record(&["", "", ""])?;
        wtr.write_record(&["Listings analyzed", &prices.len().to_string(), ""])?;
        wtr.write_record(&["Average price", &format!("{:.2}€", avg), ""])?;
        wtr.flush()?;
        println!("Results saved to average_price.csv");
        // Print summary again as the final line so it's always last in output
        println!("\n=== RESULTS ===");
        println!("Listings analyzed: {}", prices.len());
        println!("Average price: {:.2}€", avg);
        println!("Results saved to average_price.csv");
    } else {
        // Already printed above, but reprint so it's the last thing shown
        println!("\n=== RESULTS ===");
        println!("Listings analyzed: {}", prices.len());
        println!("Average price: {:.2}€", avg);
    }
    let _ = std::fs::remove_dir_all("/tmp/wallapop_scraper_profile");
    Ok(())
}
