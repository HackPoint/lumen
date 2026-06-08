pub mod compress;
pub mod meter;
pub mod rates;
pub mod record;
pub mod schema;
pub mod structure;
pub mod tokenizer;

#[cfg(test)]
mod tests {
    use crate::record::Record;

    #[test]
    fn parses_real_assistant_line() {
        let line = r#"{"parentUuid":"668c0533-9048-46e6-891c-bbac811c80d1","isSidechain":false,"message":{"model":"claude-opus-4-8","id":"msg_01VR4tRAKmGD7AcCjEXB5PVw","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"","signature":"Et4CCmMIDhgCKkCrylylnL3hxZ5vzskFl8Q31/91VMY0OR7QhkbKGryrxCzeIEK+9hcFtj8bDDS+NMl81vkM19Je/8Xu1BqiZhYYMg9jbGF1ZGUtb3B1cy00LTg4AEIIdGhpbmtpbmcSDGL/gjYvj+BiL76ySxoM/fuGt336zS7l4lEwIjC4gfe+cfTndKeL39XS042Nm5bqMHD8zTU74Ca6sTiTvxhKKx4edeF4g6SQug0+Xl0qqAEtxeku09O+1raOKpoAUqCYnEqsK9Ntn1EmRDXCvhh2WibHidCjNjMO1yD+ikMo2UObvqv+/IbCQ7RJFrAZn7VK6BT4DhCuhY2OUjkl3IMpYv+Uq0SJHfdwgYQSyilG9wrSJSwV5LY2JD0NV/trFh89teKYr9BK3R4ZsaylI0MSYVKzCdOPUCvXQ+gBSVaXdSiOHKD2LpX8XJwDoVL+ovESnJ2cfa1yWVsYAQ=="}],"stop_reason":"end_turn","stop_sequence":null,"stop_details":null,"usage":{"input_tokens":3197,"cache_creation_input_tokens":2799,"cache_read_input_tokens":16685,"output_tokens":303,"server_tool_use":{"web_search_requests":0,"web_fetch_requests":0},"service_tier":"standard","cache_creation":{"ephemeral_1h_input_tokens":2799,"ephemeral_5m_input_tokens":0},"inference_geo":"not_available","iterations":[{"input_tokens":3197,"output_tokens":303,"cache_read_input_tokens":16685,"cache_creation_input_tokens":2799,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":2799},"type":"message"}],"speed":"standard"},"diagnostics":null},"requestId":"req_011CbcqUVKoJGAWm2UVzYkVt","type":"assistant","uuid":"25cda234-8798-45fb-9669-e421b1e36ae4","timestamp":"2026-06-01T16:14:05.157Z","userType":"external","entrypoint":"claude-vscode","cwd":"/home/user/project","sessionId":"30e6df59-801a-4caf-a8f2-a355cfff7c18","version":"2.1.159","gitBranch":"master"}"#;
        let r: Record = serde_json::from_str(line).unwrap();
        assert!(r.is_billable());
    }

    #[test]
    fn count_tokens() {
        use crate::tokenizer::count_tokens;
        let n = count_tokens("Hello, world!");
        assert!(n > 0 && n < 10); // a few tokens, stable

        // chars/4 would say ~3; real BPE gives a precise count
        assert_eq!(count_tokens("Hello, world!"), count_tokens("Hello, world!")); // deterministic
    }
}
