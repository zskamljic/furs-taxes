use std::{
    collections::HashMap,
    env,
    fs::{read_to_string, File},
    io::{self, BufReader, Write},
};

use anyhow::Result;
use csv::Reader;
use serde::Deserialize;

struct Dividend {
    date: String,
    payer_id: String,
    name: String,
    address: String,
    country: String,
    amount: String,
    tax: String,
}

#[derive(Deserialize)]
struct RatesByDay {
    #[serde(rename = "$value")]
    days: Vec<Rates>,
}

#[derive(Deserialize)]
struct Rates {
    #[serde(rename = "datum")]
    date: String,
    #[serde(rename = "$value")]
    values: Vec<Rate>,
}

#[derive(Deserialize)]
struct Rate {
    #[serde(rename = "oznaka")]
    name: String,
    #[serde(rename = "$value")]
    rate: f32,
}

fn main() -> Result<()> {
    env_logger::init();

    let stdin = io::stdin();
    log::info!("Enter your tax id: ");

    let mut tax_id = String::new();
    stdin.read_line(&mut tax_id)?;

    log::info!("Loading addresses");
    let places = load_places()?;
    log::info!("Loading rates");
    let rates = load_rates()?;

    let mut args = env::args();
    let mut dividends = vec![];
    args.next();
    let revolut = args.next().unwrap();
    let revolut_dividends = load_revolut_dividends(&places, &rates, &revolut)?;
    dividends.extend(revolut_dividends);
    if let Some(t212) = args.next() {
        let t212_dividends = load_t212_dividends(&places, &rates, &t212)?;
        dividends.extend(t212_dividends);
    }

    write_output(tax_id.trim(), &dividends)?;

    Ok(())
}

fn load_places() -> Result<HashMap<String, (String, String)>> {
    let places = read_to_string("places.json")?;
    serde_json::from_str(&places).map_err(|e| e.into())
}

fn load_rates() -> Result<HashMap<String, HashMap<String, f32>>> {
    let rates = File::open("rates.xml")?;
    let rates: RatesByDay = serde_xml_rs::from_reader(rates)?;
    Ok(rates
        .days
        .iter()
        .map(|day| {
            (
                day.date.clone(),
                day.values
                    .iter()
                    .map(|rate| (rate.name.clone(), rate.rate))
                    .collect(),
            )
        })
        .collect())
}

fn load_t212_dividends(
    places: &HashMap<String, (String, String)>,
    rates: &HashMap<String, HashMap<String, f32>>,
    trading212: &str,
) -> Result<Vec<Dividend>> {
    let file = File::open(trading212)?;
    let reader = BufReader::new(file);
    let mut reader = Reader::from_reader(reader);

    let headers: HashMap<_, _> = reader
        .headers()?
        .iter()
        .enumerate()
        .map(|(i, v)| (v.to_owned(), i))
        .collect();

    let mut dividends = vec![];
    for record in reader.records() {
        let record = record?;
        if record.get(headers["Action"]) != Some("Dividend (Ordinary)") {
            continue;
        }

        let Some(date) = record.get(headers["Time"]) else {
            log::info!("Date was not present");
            continue;
        };
        let Some(isin) = record.get(headers["ISIN"]) else {
            log::info!("Payer was not present");
            continue;
        };
        let Some(name) = record.get(headers["Name"]) else {
            log::info!("Missing payer name");
            continue;
        };
        let Some(value) = record.get(headers["Total"]) else {
            log::info!("Missing dividend EUR value");
            continue;
        };
        let Some(witholding_tax) = record.get(headers["Withholding tax"]) else {
            log::info!("Missing witholding tax");
            continue;
        };
        let Some(witholding_tax_currency) = record.get(headers["Currency (Withholding tax)"])
        else {
            log::info!("Missing witholding tax currency");
            continue;
        };
        let Some(ticker) = record.get(headers["Ticker"]) else {
            log::info!("Missing ticker");
            continue;
        };
        let Some((address, country)) = company_address(ticker, places) else {
            log::error!("No address for ISIN {isin}, {ticker}, {name}");
            continue;
        };
        let Some(tax) = convert_value(date, witholding_tax, witholding_tax_currency, rates) else {
            log::error!("Did not find an exchange rate for {witholding_tax_currency}!");
            continue;
        };
        dividends.push(Dividend {
            date: date.to_owned(),
            payer_id: isin.to_owned(),
            name: name.to_owned(),
            address,
            country,
            amount: value.to_owned(),
            tax,
        });
    }
    Ok(dividends)
}

fn load_revolut_dividends(
    places: &HashMap<String, (String, String)>,
    rates: &HashMap<String, HashMap<String, f32>>,
    revolut: &str,
) -> Result<Vec<Dividend>> {
    let file = File::open(revolut)?;
    let reader = BufReader::new(file);
    let mut reader = Reader::from_reader(reader);

    let headers: HashMap<_, _> = reader
        .headers()?
        .iter()
        .enumerate()
        .map(|(i, v)| (v.to_owned(), i))
        .collect();

    let revolut_info = load_revolut_info()?;

    let mut dividends = vec![];
    for record in reader.records() {
        let record = record?;
        if record.get(headers["Type"]) != Some("DIVIDEND") {
            continue;
        }
        let Some(date) = record.get(headers["Date"]) else {
            log::error!("Missing dividend date");
            continue;
        };
        let Some(ticker) = record.get(headers["Ticker"]) else {
            log::error!("Missing ticker");
            continue;
        };
        let Some((address, country)) = company_address(ticker, places) else {
            log::error!("No address for {ticker}");
            continue;
        };
        let Some(amount) = record.get(headers["Total Amount"]) else {
            log::error!("Missing amount");
            continue;
        };
        let amount = amount.replace('$', "");
        let Some(amount) = convert_value(date, &amount, "USD", rates) else {
            log::error!("Unable to convert value");
            continue;
        };
        let Some((isin, name)) = revolut_info.get(ticker) else {
            log::error!("Missing revolut definition for {ticker}");
            continue;
        };

        dividends.push(Dividend {
            date: date.to_owned(),
            payer_id: isin.to_string(),
            name: name.to_string(),
            address,
            country,
            amount,
            tax: "0.00".to_string(),
        })
    }

    Ok(dividends)
}

fn load_revolut_info() -> Result<HashMap<String, (String, String)>> {
    let places = read_to_string("revolut.json")?;
    serde_json::from_str(&places).map_err(|e| e.into())
}

fn write_output(tax_id: &str, dividends: &[Dividend]) -> Result<()> {
    let mut output = File::create("result.csv")?;

    writeln!(
        output,
        "#FormCode;Version;TaxPayerID;TaxPayerType;DocumentWorkflowID;;;;;;\n"
    )?;
    writeln!(output, "DOH-DIV;3.9;{tax_id};FO;O;;;;;;\n")?;
    writeln!(output, "#datum prejema dividende;davčna številka izplačevalca dividend;identifikacijska  številka izplačevalca dividend;naziv izplačevalca dividend;naslov izplačevalca dividend;država izplačevalca dividend;vrsta dividende;znesek dividend;tuji davek;država vira;uveljavljam oprostitev po mednarodni pogodbi\n")?;

    for dividend in dividends {
        let Some(date) = dividend.date.split(&[' ', 'T']).next() else {
            log::error!("Unable to convert date");
            continue;
        };
        let date = date
            .split('-')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(".");
        let amount = dividend.amount.replace('.', ",");

        writeln!(
            output,
            "{};;{};{};{};{};1;{};{};{};",
            date,
            dividend.payer_id,
            dividend.name,
            dividend.address,
            dividend.country,
            amount,
            dividend.tax,
            dividend.country
        )?;
    }
    Ok(())
}

fn company_address(
    company_name: &str,
    places: &HashMap<String, (String, String)>,
) -> Option<(String, String)> {
    places.get(company_name).cloned()
}

fn convert_value(
    date: &str,
    tax: &str,
    mut currency: &str,
    rates: &HashMap<String, HashMap<String, f32>>,
) -> Option<String> {
    if currency == "EUR" {
        return Some(tax.replacen('.', ",", 1));
    }
    let mut tax: f32 = tax.parse().ok()?;
    if currency == "GBX" {
        currency = "GBP";
        tax *= 100.0;
    }

    let date = date.split(&[' ', 'T']).next()?;
    let date_rate = match rates.get(date) {
        Some(value) => value,
        None => {
            println!("Did not find currency entry for {date}");
            return None;
        }
    };
    let rate = date_rate.get(currency)?;
    Some(format!("{:.2}", tax * rate).replacen('.', ",", 1))
}
