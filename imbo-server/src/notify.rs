//! `notice` frames for the person (DESIGN §8, API-023).

use imbo_proto::frames::{Notice, ServerFrame};

use crate::state::St;

pub fn send(
    st: &St,
    account_id: i32,
    kind: &str,
    text: String,
    note: Option<String>,
    request_id: Option<i64>,
) {
    let frame = ServerFrame::Notice(Notice {
        kind: kind.to_string(),
        text,
        note,
        request_id,
    });
    st.hub.send_account(account_id, &frame);
}
