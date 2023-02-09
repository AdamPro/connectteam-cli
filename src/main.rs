#![feature(slice_group_by)]

#[macro_use]
extern crate fstrings;
extern crate chrono_english;
extern crate chrono;

use clap::Parser;
use chrono_english::{parse_date_string,Dialect};
use chrono::prelude::*;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use json::JsonValue::{self, Array, Number};
use serde::{Deserialize, Serialize};
use serde_json::json;
use term_table::row::Row;
use term_table::table_cell::TableCell;
use anyhow::{Result, Context};

struct TimesheetEntry {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    desc: String,
    project: String,
    subproject: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct SessionInfo {
    session: String,
    spirit: String,
}

#[derive(Serialize, Deserialize)]
struct TimesheetParams {
    #[serde(rename = "startDate")]
    start_date: String,

    #[serde(rename = "endDate")]
    end_date: String,

    #[serde(rename = "objectId")]
    object_id: u64,

    #[serde(rename = "defaultTimezone")]
    default_timezone: String,

    #[serde(rename = "_spirit")]
    _spirit: String,
}

#[derive(Serialize, Deserialize)]
struct PunchclockDataParams {
    #[serde(rename = "objectId")]
    object_id: u64,

    #[serde(rename = "defaultTimezone")]
    default_timezone: String,

    #[serde(rename = "_spirit")]
    _spirit: String,
}

pub trait AsVec {
    type Item;
    fn as_vec(&self) -> &Vec<Self::Item>;
}

impl AsVec for JsonValue {
    type Item = JsonValue;

    fn as_vec(&self) -> &Vec<Self::Item> {
        if let Array(elem_vec) = self {
            return elem_vec;
        }
        static EMPTY_VEC: Vec<JsonValue> = vec![];
        return &EMPTY_VEC;
    }
}

fn get_object_id_from_api(session_info: &SessionInfo) -> Result<u64> {
    let client = reqwest::blocking::Client::new();

    let resp_raw = client
        .get("https://app.connecteam.com/api/UserDashboard/ContentStructure/")
        .header(
            "cookie",
            f!("session={session_info.session}; _spirit={session_info.spirit}; "),
        )
        .send();

    let resp_raw = resp_raw.context("Sending request for content structure failed")?;
    if resp_raw.status() != 200 {
        return Err(anyhow::anyhow!(format!("Response for content structure nok: {:?}", resp_raw.status())));
    }
    let resp = resp_raw.text()?;

    let parsed = json::parse(&resp);
    let containers = &parsed.context(format!("response {:?} for content structure doesn't contain fields data:containers", resp))?["data"]["containers"];

    let object_ids = containers
        .as_vec()
        .iter()
        .filter(|x| x["name"] == "Operations")
        .flat_map(|x| x["assets"].as_vec())
        .filter(|x| x["dashboardType"] == "punchclock")
        .flat_map(|x| x["courses"].as_vec())
        .flat_map(|x| x["sections"].as_vec())
        .flat_map(|x| x["objects"].as_vec())
        .filter_map(|x| match &x["id"] {
            Number(val) => Some(val),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(object_ids.len() != 0);
    if object_ids.len() > 1 {
        println!("WARN: Found more then one matching object id!");
    }
    return Ok(object_ids[0].as_fixed_point_u64(0).unwrap());
}

fn send_request_get_timesheet(session_info: &SessionInfo, start: &DateTime<Local>, end: &DateTime<Local>) -> Result<String> {
    let client = reqwest::blocking::Client::new();

    let request_payload = TimesheetParams {
        start_date: start.format("%Y-%m-%d").to_string(),
        end_date: end.format("%Y-%m-%d").to_string(),
        object_id: get_object_id_from_api(&session_info).context("getting object id failed")?,
        default_timezone: "Europe/Warsaw".to_string(),
        _spirit: session_info.spirit.clone(),
    };

    let resp_raw = client
        .post("https://app.connecteam.com/api/UserDashboard/PunchClock/Timesheet/")
        .header(
            "cookie",
            f!("session={session_info.session}; _spirit={session_info.spirit}; "),
        )
        .body(json!(request_payload).to_string())
        .send();
    
    let resp_raw = resp_raw.with_context(|| format!("Response for get timesheet nok "))?;
    let result = resp_raw.text().with_context(|| format!("Response for get timesheet nok "))?;
    return Ok(result);
}

fn parse_timesheet(resp: String) -> Result<Vec<TimesheetEntry>> {
    let parsed = json::parse(&resp);
    let time_sheet_entries = &parsed?["data"]["userTimeSheets"]["timeSheetEntries"];

    let timesheet_entries = time_sheet_entries
        .as_vec()
        .iter()
        .flat_map(|x| x["timeSheetDayEntries"].as_vec())
        .flat_map(|x| x["shifts"].as_vec())
        .map(|shift| {
            let parse_timestamp = |timestamp: &JsonValue| {
                let seconds_since_epoch = timestamp["timestampWithTimezone"]["timestamp"]
                    .as_i64()
                    .unwrap();
                return Utc.timestamp_opt(seconds_since_epoch, 0).unwrap();
            };
            
            let mut desc = "".to_string();
            let free_text = shift["shiftAttachments"].as_vec()[0]["freeText"].to_string();
            let notes = shift["employeeNotes"].to_string();

            if !(free_text == "" || free_text == "null") && notes != "" {
                desc = f!("{free_text} / {notes}");
            } else if !(free_text == "" || free_text == "null") {
                desc = free_text;
            } else if notes != "" {
                desc = notes;
            }

            TimesheetEntry {
                start: parse_timestamp(&shift["punchIn"]),
                end: parse_timestamp(&shift["punchOut"]),
                desc: desc,
                project: shift["punchTag"]["name"].to_string(),
                subproject: shift["punchTag"]["subItems"][0]["name"].to_string(),
            }
        })
        .collect();

    return Ok(timesheet_entries);
}


fn load_session_info_or_ask_user() -> Result<SessionInfo> {
    let mut session_info_file = home::home_dir().unwrap();
    session_info_file.push(".config/connectteam.json");

    if session_info_file.exists() {
        let info_json = std::fs::read_to_string(session_info_file)?;
        let session_info: SessionInfo = serde_json::from_str(&info_json)?;
        return Ok(session_info);
    } else {
        println!("Session information are not stored in {}. Please go to https://app.connecteam.com/, login in, open developer console \
        (ctrl+shift+c in most browsers), go to network, open time clock page in the browsers, navigate to Timesheet request, copy cookie values from request header, copy response to clipboard and past here:", session_info_file.to_str().unwrap());

        let mut user_input = String::new();
        let stdin = std::io::stdin();
        stdin.read_line(&mut user_input)?;

        let mut user_input = user_input.trim().to_string();
        if user_input.starts_with("'") {
            user_input.remove(0);
        }
        if user_input.ends_with("'") {
            user_input.remove(user_input.len() - 1);
        }

        let extract_field_from_cookie = |field| {
            user_input
                .split(";")
                .map(|x| x.split("=").map(|x| x.trim()).collect::<Vec<_>>())
                .filter(|x| x.len() == 2)
                .filter(|x| x[0] == field)
                .flatten()
                .collect::<Vec<_>>()[1]
        };

        let session_info = SessionInfo {
            session: extract_field_from_cookie("session").to_string(),
            spirit: extract_field_from_cookie("_spirit").to_string(),
        };

        std::fs::write(
            session_info_file,
            serde_json::to_string_pretty(&session_info).unwrap(),
        )?;
        return Ok(session_info);
    }
}

fn draw_timesheet(entries: &mut Vec<TimesheetEntry>) {
    entries.sort_by_key(|k| k.start);
    entries.reverse();

    let grouped: Vec<_> = entries
        .group_by(|k, l| {
            k.start.day() == l.start.day()
                && k.start.month() == l.start.month()
                && k.start.year() == l.start.year()
        })
        .collect();

    let mut table = term_table::Table::new();
    table.max_column_width = 120;
    table.style = term_table::TableStyle::extended();

    table.add_row(Row::new(vec![
        TableCell::new("Start"),
        TableCell::new("End"),
        TableCell::new("Description"),
        TableCell::new("Project"),
        TableCell::new("Subproject"),
    ]));
    for day in grouped {
        table.add_row(Row::new(vec![TableCell::new_with_alignment(
            day.first().unwrap().start.date_naive(),
            5,
            term_table::table_cell::Alignment::Center,
        )]));
        for entry in day {
            table.add_row(Row::new(vec![
                TableCell::new(entry.start.time().format("%H:%M")),
                TableCell::new(entry.end.time().format("%H:%M")),
                TableCell::new(&entry.desc),
                TableCell::new(&entry.project),
                TableCell::new(&entry.subproject),
            ]));
        }
    }
    println!("{}", table.render());
}

#[derive(Parser)]
struct Cli {
    #[clap(short, long, default_value = "7 days ago")]
    start: String,

    #[clap(short, long, default_value = "today")]
    end: String,
}

fn main() -> Result<()> {
    let args = Cli::parse();
    let start = parse_date_string(&args.start, Local::now(), Dialect::Uk).with_context(|| f!("Failed to parse start date: {args.start}"))?;
    let end = parse_date_string(&args.end, Local::now(), Dialect::Uk).with_context(|| f!("Failed to parse end date: {args.end}"))?;
    println!("Looking for entries in between: {} - {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"));

    let session_info = load_session_info_or_ask_user().with_context(|| "Failed to load session")?;

    let resp = send_request_get_timesheet(&session_info, &start, &end).with_context(|| "Failed to send request \"get timesheet\"")?;
    let mut entries = parse_timesheet(resp).with_context(|| "Failed to parse request")?;

    draw_timesheet(&mut entries);
    Ok(())
}
