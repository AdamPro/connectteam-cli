#![feature(slice_group_by)]

#[macro_use]
extern crate fstrings;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use json::JsonValue::{self, Array, Number};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::error::Error;
use term_table::row::Row;
use term_table::table_cell::TableCell;

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

fn get_object_id_from_api(session_info: &SessionInfo) -> Result<u64, Box<dyn Error>> {
    let client = reqwest::blocking::Client::new();

    let resp_raw = client
        .get("https://app.connecteam.com/api/UserDashboard/ContentStructure/")
        .header(
            "cookie",
            f!("session={session_info.session}; _spirit={session_info.spirit}; "),
        )
        .send();
    let resp = resp_raw?.text()?;
    let parsed = json::parse(&resp);
    let containers = &parsed?["data"]["containers"];

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

fn send_request_get_timesheet(session_info: &SessionInfo) -> Result<String, Box<dyn Error>> {
    let client = reqwest::blocking::Client::new();

    let request_payload = TimesheetParams {
        start_date: "2023-02-01".to_string(),
        end_date: "2023-02-28".to_string(),
        object_id: get_object_id_from_api(&session_info)?,
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

    return Ok(resp_raw?.text()?);
}

fn send_request_get_punchclock_data(session_info: &SessionInfo) -> Result<String, Box<dyn Error>> {
    let client = reqwest::blocking::Client::new();

    let request_payload = PunchclockDataParams {
        object_id: get_object_id_from_api(&session_info)?,
        default_timezone: "Europe/Warsaw".to_string(),
        _spirit: session_info.spirit.clone(),
    };

    let resp_raw = client
        .post("https://app.connecteam.com/api/UserDashboard/PunchClock/Data/")
        .header(
            "cookie",
            f!("session={session_info.session}; _spirit={session_info.spirit}; "),
        )
        .body(json!(request_payload).to_string())
        .send();

    return Ok(resp_raw?.text()?);
}

fn parse_timesheet(resp: String) -> Result<Vec<TimesheetEntry>, Box<dyn Error>> {
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

fn parse_punchclock(resp: String) -> Result<(), Box<dyn Error>> {
    let parsed = json::parse(&resp)?;

    let shift_attachment = &parsed["data"]["punchClockSettings"]["shiftAttachments"]
        .as_vec()
        .iter()
        .map(|x| {
            (
                x["id"].to_string(),
                x["name"].to_string(),
                x["type"].to_string(),
            )
        })
        .collect::<Vec<_>>();

    println!("{:?}", shift_attachment);

    let available_tags = &parsed["data"]["availableTags"]
        .as_vec()
        .iter()
        .flat_map(|x| {
            let sub_items = 
            x["subItems"]
                .as_vec();
            if sub_items.is_empty() {
                return vec![(x["name"].to_string(), "".to_string())];
            }
            else {
                return sub_items.iter().map(|y| (x["name"].to_string(), y["name"].to_string())).collect();
            }
        })
        .collect::<Vec<_>>();

    println!("{:?}", available_tags);

    return Ok(());
}

fn load_session_info_or_ask_user() -> Result<SessionInfo, Box<dyn Error>> {
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


fn main() -> Result<(), Box<dyn Error>> {
    let session_info = load_session_info_or_ask_user()?;

    let resp = send_request_get_timesheet(&session_info)?;
    let mut entries = parse_timesheet(resp)?;

    let punchlock_data_resp = send_request_get_punchclock_data(&session_info)?;
    parse_punchclock(punchlock_data_resp)?;
    // println!("{:?}", punchlock_data_resp);

    // println!("{:?}", json::parse(&punchlock_data_resp)?);

    draw_timesheet(&mut entries);
    Ok(())
}
