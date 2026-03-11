use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;
use std::io;
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

async fn scrape_prices(page: &chromiumoxide::Page) -> Vec<(String, f64, String)> {
    let js = r#"
        (function() {
            const results = [];
            const priceEls = document.querySelectorAll('strong[aria-label="Item price"]');
            console.log('[scrape] Found price elements: ' + priceEls.length);

            priceEls.forEach(el => {
                // Walk up to find the anchor tag (the whole card is usually an <a>)
                let card = el.closest('a[href]');
                let url = 'unknown';
                if (card) {
                    url = card.href; // absolute URL
                } else {
                    // fallback: find nearest anchor in parent
                    let parent = el.parentElement;
                    for (let i = 0; i < 6; i++) {
                        if (!parent) break;
                        const a = parent.querySelector('a[href]');
                        if (a) { url = a.href; break; }
                        parent = parent.parentElement;
                    }
                }

                // Get title
                let title = 'unknown';
                const container = el.closest('[class*="ItemCard"]') || el.closest('a') || el.parentElement;
                if (container) {
                    const titleEl = container.querySelector('h3[class*="title"], h3[class*="Title"]')
                        || container.querySelector('h3');
                    if (titleEl) title = titleEl.textContent.trim();
                }

                // Parse price: "130 €" or "1.200 €"
                const raw = el.textContent.replace(/\u00a0/g, ' ').trim();
                const numeric = raw.replace(/[^\d,.]/g, '').replace('.', '').replace(',', '.');
                const price = parseFloat(numeric);

                if (!isNaN(price)) {
                    results.push({ title, price, url });
                } else {
                    console.log('[scrape] Could not parse price from: ' + raw);
                }
            });

            return JSON.stringify(results);
        })()
    "#;

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
                        let title = item["title"].as_str().unwrap_or("unknown").to_string();
                        let price = item["price"].as_f64()?;
                        let url = item["url"].as_str().unwrap_or("unknown").to_string();
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
    let js = r#"
        (function() {
            const wallaBtn = document.querySelector('walla-button[text="Cargar más"]');
            if (wallaBtn && wallaBtn.shadowRoot) {
                const btn = wallaBtn.shadowRoot.querySelector('button');
                if (btn) { btn.click(); return true; }
            }
            const allBtns = document.querySelectorAll('walla-button');
            for (const wb of allBtns) {
                if (wb.getAttribute('text') === 'Cargar más') {
                    if (wb.shadowRoot) {
                        const btn = wb.shadowRoot.querySelector('button');
                        if (btn) { btn.click(); return true; }
                    }
                    wb.click();
                    return true;
                }
            }
            return false;
        })()
    "#;
    match page.evaluate(js).await {
        Ok(result) => result.value().and_then(|v| v.as_bool()).unwrap_or(false),
        Err(_) => false,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Search keyword:");
    let mut keyword = String::new();
    io::stdin().read_line(&mut keyword)?;
    let keyword = keyword.trim().to_string();

    println!("Max listings to analyze:");
    let mut limit = String::new();
    io::stdin().read_line(&mut limit)?;
    let limit: usize = limit.trim().parse().unwrap_or(50);

    let browser_path = find_chromium().expect("No Chromium browser found");
    println!("Using browser: {}", browser_path);

    let (mut browser, mut handler) = Browser::launch(
        BrowserConfig::builder()
            .chrome_executable(browser_path)
            .args(vec![
                "--no-sandbox",
                "--disable-dev-shm-usage",
                "--disable-gpu",
                "--lang=es",
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
    sleep(Duration::from_secs(4)).await;
    dismiss_popups(&page).await;
    sleep(Duration::from_secs(1)).await;

    let mut prices: Vec<f64> = Vec::new();
    let mut titles: Vec<String> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        let found = scrape_prices(&page).await;
        println!("[scrape] Got {} items from DOM", found.len());

        for (title, price, url) in found {
            let key = format!("{}:{}", title, price);
            if !seen.contains(&key) {
                seen.insert(key);
                println!("  + '{}' => {:.2}€  ({})", title, price, url);
                prices.push(price);
                titles.push(title);
                urls.push(url);
            }
        }

        println!("[loop] Unique collected: {}/{}", prices.len(), limit);
        if prices.len() >= limit {
            break;
        }

        println!("[loop] Clicking 'Cargar más'...");
        let clicked = click_load_more(&page).await;
        if !clicked {
            println!("[loop] No 'Cargar más' button found, done.");
            break;
        }

        println!("[loop] Waiting for new items to render...");
        sleep(Duration::from_secs(3)).await;
    }

    browser.close().await?;

    prices.truncate(limit);
    titles.truncate(limit);
    urls.truncate(limit);

    let sum: f64 = prices.iter().sum();
    let avg = sum / prices.len() as f64;

    println!("\n=== RESULTS ===");
    println!("Listings analyzed: {}", prices.len());
    println!("Average price: {:.2}€", avg);

    println!("\nDo you want to download the results? (y/n)");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;

    if answer.trim().to_lowercase() == "y" {
        let mut wtr = csv::Writer::from_path("wallapop_results.csv")?;
        wtr.write_record(["title", "price", "url"])?;
        for i in 0..prices.len() {
            wtr.write_record(&[
                titles.get(i).unwrap_or(&"unknown".to_string()),
                &prices[i].to_string(),
                urls.get(i).unwrap_or(&"unknown".to_string()),
            ])?;
        }
        wtr.flush()?;
        println!("Results saved to wallapop_results.csv");
    }

    Ok(())
}
