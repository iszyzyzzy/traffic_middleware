#[macro_use]
extern crate rocket;

use chrono::{Datelike, Local, TimeZone};
use rocket::serde::json::Json;
use serde::{Deserialize, Serialize};
use serde_yaml;
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Serialize, Deserialize)]
enum UnitType {
    Decimal,
    Binary,
}

#[derive(Debug, Serialize, Deserialize)]
struct LimitLoad {
    reset_day: u8,
    limit: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ConfigLoads {
    prometheus_url: String,
    unit_type: UnitType,
    limits: HashMap<String, LimitLoad>,
}

#[derive(Debug)]
struct Limit {
    reset_day: u8,
    limit: u64,
}

#[derive(Debug)]
struct Config {
    prometheus_url: String,
    unit_type: UnitType,
    limits: HashMap<String, Limit>,
}

// 将形如"1tb"的字符串转换为字节数
fn byte_conversion(s: &str, unit_type: &UnitType) -> Option<u64> {
    let mut num = String::new();
    let mut unit = String::new();
    for c in s.chars() {
        if c.is_digit(10) {
            num.push(c);
        } else {
            unit.push(c);
        }
    }
    let num = num.parse::<u64>().unwrap();
    match unit_type {
        UnitType::Decimal => match unit.as_str() {
            "b" | "B" => Some(num),
            "kb" | "KB" => Some(num * 1000),
            "mb" | "MB" => Some(num * 1000 * 1000),
            "gb" | "GB" => Some(num * 1000 * 1000 * 1000),
            "tb" | "TB" => Some(num * 1000 * 1000 * 1000 * 1000),
            _ => None,
        },
        UnitType::Binary => match unit.as_str() {
            "b" | "B" => Some(num),
            "kb" | "KB" => Some(num * 1024),
            "mb" | "MB" => Some(num * 1024 * 1024),
            "gb" | "GB" => Some(num * 1024 * 1024 * 1024),
            "tb" | "TB" => Some(num * 1024 * 1024 * 1024 * 1024),
            _ => None,
        },
    }
}

fn read_config() -> Config {
    let config_str = fs::read_to_string("config.yml").unwrap();
    let config: ConfigLoads = serde_yaml::from_str(&config_str).unwrap();
    let config = Config {
        prometheus_url: config.prometheus_url,
        limits: config
            .limits
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    Limit {
                        reset_day: v.reset_day,
                        limit: byte_conversion(&v.limit, &config.unit_type).unwrap(),
                    },
                )
            })
            .collect(),
        unit_type: config.unit_type,
    };
    println!("{:?}", config);
    config
}

fn get_seconds_from_rest_day(reset_day: &u8) -> i64 {
    let now = Local::now();
    if now.day() < *reset_day as u32 {
        let last_day = Local
            .with_ymd_and_hms(now.year(), now.month() - 1, *reset_day as u32, 0, 0, 0)
            .unwrap();
        let duration = now.signed_duration_since(last_day);
        duration.num_seconds()
    } else {
        let last_day = Local
            .with_ymd_and_hms(now.year(), now.month(), *reset_day as u32, 0, 0, 0)
            .unwrap();
        let duration = now.signed_duration_since(last_day);
        duration.num_seconds()
    }
}

#[get("/")]
fn index() -> &'static str {
    "Hello, world!"
}

#[derive(Serialize)]
struct RawLoad {
    value: f64,
    limit: u64,
}

#[derive(Deserialize)]
struct SourcePayload {
    data: SourceData,
}

#[derive(Deserialize)]
struct SourceData {
    result: Vec<SourceResult>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SourceResultValue {
    String(String),
    F64(f64),
}

trait AsF64 {
    fn as_f64(&self) -> f64;
}

impl AsF64 for SourceResultValue {
    fn as_f64(&self) -> f64 {
        match self {
            SourceResultValue::String(s) => s.parse().unwrap(),
            SourceResultValue::F64(f) => *f,
        }
    }
}

#[derive(Deserialize)]
struct SourceResult {
    value: Vec<SourceResultValue>,
}

#[derive(Deserialize)]
struct LabelValuePayload {
    data: Vec<String>,
}

fn generate_url(prometheus_url: &String, reset_day: &u8, instance: &String) -> String {
    let seconds = get_seconds_from_rest_day(reset_day);
    format!("{}/api/v1/query?query=sum (increase(node_network_receive_bytes_total{{instance=\"{}\"}}[{}s]) %2B increase(node_network_transmit_bytes_total{{instance=\"{}\"}}[{}s]))",&prometheus_url,&instance,&seconds,&instance,&seconds)
}

async fn get_raw_data(
    config: &Config,
    http_client: &reqwest::Client,
) -> HashMap<String, RawLoad> {
    let mut return_data: HashMap<String, RawLoad> = HashMap::new();
    let limit_fallback = Limit {
        reset_day: 1,
        limit: byte_conversion("9999tb", &config.unit_type).unwrap(),
    };
    let mut instance_list = http_client
        .get(&format!(
            "{}/api/v1/label/job/values",
            &config.prometheus_url
        ))
        .send()
        .await
        .unwrap()
        .json::<LabelValuePayload>()
        .await
        .unwrap()
        .data;
    instance_list.retain(|x| x != "Node Exporter");
    for instance in instance_list {
        let limit = config
            .limits
            .get(&instance)
            .unwrap_or(&limit_fallback);
        let url = generate_url(
            &config.prometheus_url,
            &limit.reset_day,
            &instance,
        );
        let data = http_client
            .get(&url)
            .send()
            .await
            .unwrap()
            .json::<SourcePayload>()
            .await
            .unwrap();
        if data.data.result.len() > 0 {
            return_data.insert(
                instance,
                RawLoad {
                    value: data.data.result[0].value[1].as_f64(),
                    limit: limit.limit,
                },
            );
        }
    }
    return_data
}

#[get("/get/raw")]
async fn get_data(
    config: &rocket::State<Config>,
    http_client: &rocket::State<reqwest::Client>,
) -> Json<HashMap<String, RawLoad>> {
    Json(get_raw_data(&config, &http_client).await)
}

#[get("/get/precentage")]
async fn get_precent(
    config: &rocket::State<Config>,
    http_client: &rocket::State<reqwest::Client>,
) -> Json<HashMap<String, f64>> {
    let return_data = get_raw_data(&config, &http_client)
        .await
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                v.value / v.limit as f64 * 100.0,
            )
        })
        .collect();
    Json(return_data)
}

#[launch]
fn rocket() -> _ {
    let config = read_config();
    let http_client = reqwest::Client::builder().build().unwrap();
    rocket::build()
        .mount("/", routes![index])
        .mount("/", routes![get_data])
        .mount("/", routes![get_precent])
        .manage(config)
        .manage(http_client)
}
