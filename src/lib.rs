use pgrx::pg_sys::panic::ErrorReport;
use pgrx::warning;
use pgrx::PgSqlErrorCode;
use pgrx::{pg_sys, prelude::*, JsonB};

use std::collections::HashMap;
use std::env;
use tokio::runtime::Runtime;

use serde_json::Value as JsonValue;
use std::str::FromStr;
use supabase_wrappers::prelude::*;
pgrx::pg_module_magic!();

use clerk_rs::{
    apis::organization_memberships_api::OrganizationMembership,
    apis::organizations_api::Organization, apis::users_api::User, clerk::Clerk, ClerkConfiguration,
};

// TODO: will have to incorporate offset at some point
const PAGE_SIZE: usize = 500;

fn body_to_rows(
    resp: &JsonValue,
    obj_key: &str,
    normal_cols: Vec<(&str, &str, &str)>,
    tgt_cols: &[Column],
) -> Vec<Row> {
    let mut result = Vec::new();

    let objs = if resp.is_array() {
        // If `resp` is directly an array
        resp.as_array().unwrap()
    } else {
        // If `resp` is an object containing the array under `obj_key`
        match resp
            .as_object()
            .and_then(|v| v.get(obj_key))
            .and_then(|v| v.as_array())
        {
            Some(objs) => objs,
            None => return result,
        }
    };

    for obj in objs {
        let mut row = Row::new();

        // extract normal columns
        for tgt_col in tgt_cols {
            if let Some((src_name, col_name, col_type)) =
                normal_cols.iter().find(|(_, c, _)| c == &tgt_col.name)
            {
                // Navigate through nested properties
                let mut current_value: Option<&JsonValue> = Some(obj);
                for part in src_name.split('.') {
                    current_value = current_value.unwrap().as_object().unwrap().get(part);
                }

                if *src_name == "email_addresses" {
                    current_value = current_value
                        .and_then(|v| v.as_array().and_then(|arr| arr.first()))
                        .and_then(|first_obj| {
                            first_obj
                                .as_object()
                                .and_then(|obj| obj.get("email_address"))
                        });
                }

                let cell = current_value.and_then(|v| match *col_type {
                    "bool" => v.as_bool().map(Cell::Bool),
                    "i64" => v.as_i64().map(Cell::I64),
                    "string" => v.as_str().map(|a| Cell::String(a.to_owned())),
                    "timestamp" => v.as_str().map(|a| {
                        let secs = a.parse::<i64>().unwrap() / 1000;
                        let ts = to_timestamp(secs as f64);
                        Cell::Timestamp(ts.to_utc())
                    }),
                    "timestamp_iso" => v.as_str().map(|a| {
                        let ts = Timestamp::from_str(a).unwrap();
                        Cell::Timestamp(ts)
                    }),
                    "json" => Some(Cell::Json(JsonB(v.clone()))),
                    _ => None,
                });
                row.push(col_name, cell);
            }
        }

        // put all properties into 'attrs' JSON column
        if tgt_cols.iter().any(|c| &c.name == "attrs") {
            let attrs = serde_json::from_str(&obj.to_string()).unwrap();
            row.push("attrs", Some(Cell::Json(JsonB(attrs))));
        }

        result.push(row);
    }
    result
}

// convert response body text to rows
fn resp_to_rows(obj: &str, resp: &JsonValue, tgt_cols: &[Column]) -> Vec<Row> {
    let mut result = Vec::new();

    match obj {
        "users" => {
            result = body_to_rows(
                resp,
                "data",
                vec![
                    ("id", "user_id", "string"),
                    ("first_name", "first_name", "string"),
                    ("last_name", "last_name", "string"),
                    ("email_addresses", "email", "string"),
                    ("gender", "gender", "string"),
                    ("created_at", "created_at", "i64"),
                    ("updated_at", "updated_at", "i64"),
                    ("last_sign_in_at", "last_sign_in_at", "i64"),
                    ("phone_numbers", "phone_numbers", "i64"),
                    ("username", "username", "string"),
                ],
                tgt_cols,
            );
        }
        "organizations" => {
            result = body_to_rows(
                resp,
                "data",
                vec![
                    ("id", "organization_id", "string"),
                    ("name", "name", "string"),
                    ("slug", "slug", "string"),
                    ("created_at", "created_at", "i64"),
                    ("updated_at", "updated_at", "i64"),
                    ("created_by", "created_by", "string"),
                ],
                tgt_cols,
            );
        }
        "organization_memberships" => {
            result = body_to_rows(
                resp,
                "data",
                vec![
                    ("public_user_data.user_id", "user_id", "string"),
                    ("organization.id", "organization_id", "string"),
                    ("role", "role", "string"),
                ],
                tgt_cols,
            );
        }
        _ => {
            warning!("unsupported object: {}", obj);
        }
    }

    result
}

#[wrappers_fdw(
    version = "0.3.0",
    author = "Tembo.io",
    website = "https://tembo.io",
    error_type = "ClerkFdwError"
)]
pub(crate) struct ClerkFdw {
    rt: Runtime,
    scan_result: Option<Vec<Row>>,
    tgt_cols: Vec<Column>,
    clerk_client: Clerk,
}

enum ClerkFdwError {}

impl From<ClerkFdwError> for ErrorReport {
    fn from(_value: ClerkFdwError) -> Self {
        ErrorReport::new(PgSqlErrorCode::ERRCODE_FDW_ERROR, "", "")
    }
}

type ClerkFdwResult<T> = Result<T, ClerkFdwError>;

impl ForeignDataWrapper<ClerkFdwError> for ClerkFdw {
    fn new(options: &HashMap<String, String>) -> ClerkFdwResult<Self> {
        let token = if let Some(access_token) = options.get("api_key") {
            access_token.to_owned()
        } else {
            warning!("Cannot find api_key in options");
            env::var("CLERK_API_KEY").unwrap()
        };

        let clerk_client = Clerk::new(ClerkConfiguration::new(
            None,
            None,
            Some(token.to_string()),
            None,
        ));
        let rt = create_async_runtime().expect("failed to create async runtime");
        Ok(Self {
            rt,
            tgt_cols: Vec::new(),
            scan_result: None,
            clerk_client,
        })
    }

    fn begin_scan(
        &mut self,
        _quals: &[Qual],
        columns: &[Column],
        _sorts: &[Sort],
        _limit: &Option<Limit>,
        options: &HashMap<String, String>,
    ) -> ClerkFdwResult<()> {
        let obj = require_option("object", options).expect("invalid option");

        self.scan_result = None;
        self.tgt_cols = columns.to_vec();

        let mut result = Vec::new();
        let run = self.rt.block_on(async {
            if obj == "organization_memberships" {
                // Get all organizations first
                let mut offset: f32 = 0.0;
                loop {
                    let org_resp = Organization::list_organizations(
                        &self.clerk_client,
                        Some(PAGE_SIZE as f32),
                        Some(offset),
                        None,
                        None,
                    )
                    .await;

                    if let Ok(org_res) = org_resp {
                        for org in org_res.data.iter() {
                            let membership_resp =
                                OrganizationMembership::list_organization_memberships(
                                    &self.clerk_client,
                                    &org.id,
                                    Some(PAGE_SIZE as f32),
                                    Some(offset),
                                )
                                .await;

                            match membership_resp {
                                Ok(mem_res) => {
                                    let serde_v = serde_json::to_value(mem_res).unwrap();
                                    let mut rows = resp_to_rows(obj, &serde_v, &self.tgt_cols[..]);
                                    result.append(&mut rows);
                                }
                                Err(e) => {
                                    warning!(
                                        "Failed to get memberships for organization: {}, error: {}",
                                        &org.id,
                                        e
                                    );
                                    continue;
                                }
                            }
                            // Introduce a delay of 0.05 seconds
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        if org_res.data.len() < PAGE_SIZE {
                            break;
                        } else {
                            offset += PAGE_SIZE as f32;
                        }
                    } else {
                        warning!("Failed to get organizations. error: {:#?}", org_resp);
                    }
                }
            } else {
                // this is where i need to make changes
                let mut offset = 0;
                loop {
                    let obj_js =
                        match obj {
                            "users" => {
                                match User::get_user_list(
                                    &self.clerk_client,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    Some(PAGE_SIZE as f32),
                                    Some(offset as f32),
                                    None,
                                )
                                .await
                                {
                                    Ok(users) => serde_json::to_value(users)
                                        .expect("failed deserializing users"),
                                    Err(e) => {
                                        warning!("Failed to get users: {}", e);
                                        break;
                                    }
                                }
                            }
                            "organizations" => {
                                match Organization::list_organizations(
                                    &self.clerk_client,
                                    Some(PAGE_SIZE as f32),
                                    Some(offset as f32),
                                    None,
                                    None,
                                )
                                .await
                                {
                                    Ok(orgs) => serde_json::to_value(orgs)
                                        .expect("failed deserializing orgs"),
                                    Err(e) => {
                                        warning!("Failed to get organizations: {}", e);
                                        break;
                                    }
                                }
                                //
                            }
                            _ => {
                                warning!("unsupported object: {}", obj);
                                return Err(());
                            }
                        };

                    let mut rows = resp_to_rows(obj, &obj_js, &self.tgt_cols[..]);
                    if rows.len() < PAGE_SIZE {
                        result.append(&mut rows);
                        break;
                    } else {
                        result.append(&mut rows);
                        offset += PAGE_SIZE;
                    }
                }
            }
            Ok(())
        });
        run.expect("failed to run async block");
        self.scan_result = Some(result);
        Ok(())
    }

    fn iter_scan(&mut self, row: &mut Row) -> ClerkFdwResult<Option<()>> {
        if let Some(ref mut result) = self.scan_result {
            if !result.is_empty() {
                let scanned = result
                    .drain(0..1)
                    .last()
                    .map(|src_row| row.replace_with(src_row));
                return Ok(scanned);
            }
        }
        Ok(None)
    }

    fn end_scan(&mut self) -> ClerkFdwResult<()> {
        self.scan_result.take();
        Ok(())
    }

    fn validator(options: Vec<Option<String>>, catalog: Option<pg_sys::Oid>) -> ClerkFdwResult<()> {
        if let Some(oid) = catalog {
            if oid == FOREIGN_TABLE_RELATION_ID {
                check_options_contain(&options, "object").expect("missing object option");
            }
        }
        Ok(())
    }
}
