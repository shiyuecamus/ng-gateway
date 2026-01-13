use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};

pub fn encode_url_component(component: &str) -> String {
    utf8_percent_encode(component, NON_ALPHANUMERIC).to_string()
}
