use base64::Engine;

const SETUP_HTML_TEMPLATE: &str = include_str!("setup.html");
const SETUP_EVENT_JS: &str = include_str!("setup-event.js");

pub fn get_setup_html(recovery_message: &str) -> String {
    let standalone_label = if recovery_message.is_empty() {
        "Use local Foreseer"
    } else {
        "Retry local Foreseer"
    };
    let recovery = if recovery_message.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="status-box error" role="alert">{}</div>"#,
            escape_html(recovery_message)
        )
    };

    SETUP_HTML_TEMPLATE
        .replace("{{RECOVERY_MESSAGE}}", &recovery)
        .replace("{{STANDALONE_ACTION_LABEL}}", standalone_label)
        .replace("{{SETUP_EVENT_JS}}", SETUP_EVENT_JS)
}

pub fn setup_document_url(recovery_message: &str) -> String {
    let html = get_setup_html(recovery_message);
    format!(
        "data:text/html;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(html.as_bytes())
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::{get_setup_html, setup_document_url};

    #[test]
    fn setup_document_url_is_a_bounded_data_document() {
        let url = setup_document_url("");
        assert!(url.starts_with("data:text/html;base64,"));
        assert!(url.len() <= 256 * 1024);
    }

    #[test]
    fn setup_page_embeds_the_typed_protocol_listener() {
        let html = get_setup_html("");
        assert!(html.contains("foreseerSetupProtocolV1"));
        assert!(html.contains("detail.status"));
        assert!(html.contains("detail.message"));
        assert!(html.contains("foreseerNative"));
        assert!(html.contains("setup.standalone"));
        assert!(html.contains("Use local Foreseer"));
        assert!(html.contains("Connect"));
        assert!(html.contains("Quit"));
        assert!(!html.contains("{{SETUP_EVENT_JS}}"));
        assert!(!html.contains("{{RECOVERY_MESSAGE}}"));
        assert!(!html.contains("{{STANDALONE_ACTION_LABEL}}"));
        assert!(!html.contains("jelliumHost"));
    }

    #[test]
    fn setup_page_escapes_startup_recovery_errors() {
        let html = get_setup_html("Unable to start <script>alert('xss')</script>");

        assert!(html.contains("Unable to start &lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert('xss')</script>"));
        assert!(html.contains("Retry local Foreseer"));
    }
}
