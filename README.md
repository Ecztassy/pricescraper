# Wallapop Price Scraper

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)](https://www.rust-lang.org/)

A simple price scraper for Wallapop that retrieves prices directly via JavaScript and calculates an average.  

Currently in use by [Reciclanet](https://reciclanet.org/).  

## Features
- Fetches Wallapop prices via JS.  
- Calculates average prices automatically.
## TODO
- Other sources (Backmarket, Amazon, etc).
- UI in Slint [Slint](https://github.com/slint-ui/slint).

## Installation

Make sure you have Rust installed

```bash
git clone https://github.com/Ecztassy/pricescraper.git
cd pricescraper
cargo build --release
