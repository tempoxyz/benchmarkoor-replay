use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: Option<String>,
    pub id: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

pub fn parse_request_line(line: &str) -> Result<JsonRpcRequest> {
    serde_json::from_str(line).with_context(|| "parsing JSON-RPC request line")
}

pub fn validate_engine_response(method: &str, response: &JsonRpcResponse) -> Result<()> {
    if let Some(error) = &response.error {
        anyhow::bail!("JSON-RPC error {}: {}", error.code, error.message);
    }

    if method.starts_with("engine_newPayload") {
        let result = response
            .result
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine_newPayload response has no result"))?;
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("engine_newPayload response missing result.status"))?;
        if status != "VALID" {
            let validation = result
                .get("validationError")
                .and_then(Value::as_str)
                .unwrap_or("");
            if validation.is_empty() {
                anyhow::bail!("newPayload status is {status}, expected VALID");
            }
            anyhow::bail!("newPayload status is {status}, expected VALID: {validation}");
        }
    }

    if method.starts_with("engine_forkchoiceUpdated") {
        let result = response
            .result
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("engine_forkchoiceUpdated response has no result"))?;
        let status = result
            .get("payloadStatus")
            .and_then(|v| v.get("status"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "engine_forkchoiceUpdated response missing result.payloadStatus.status"
                )
            })?;
        if status != "VALID" {
            let validation = result
                .get("payloadStatus")
                .and_then(|v| v.get("validationError"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if validation.is_empty() {
                anyhow::bail!("forkchoiceUpdated status is {status}, expected VALID");
            }
            anyhow::bail!("forkchoiceUpdated status is {status}, expected VALID: {validation}");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_jsonrpc_errors() {
        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid Request"}}"#,
        )
        .unwrap();
        let err = validate_engine_response("engine_newPayloadV3", &response).unwrap_err();
        assert!(err.to_string().contains("JSON-RPC error -32600"));
    }

    #[test]
    fn validates_new_payload_status() {
        let response: JsonRpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"status":"VALID"}}"#)
                .unwrap();
        validate_engine_response("engine_newPayloadV3", &response).unwrap();

        let response: JsonRpcResponse =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"status":"SYNCING"}}"#)
                .unwrap();
        assert!(validate_engine_response("engine_newPayloadV3", &response).is_err());
    }

    #[test]
    fn validates_forkchoice_status() {
        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"result":{"payloadStatus":{"status":"VALID"}}}"#,
        )
        .unwrap();
        validate_engine_response("engine_forkchoiceUpdatedV3", &response).unwrap();

        let response: JsonRpcResponse = serde_json::from_str(
            r#"{"jsonrpc":"2.0","id":1,"result":{"payloadStatus":{"status":"INVALID","validationError":"unknown ancestor"}}}"#,
        )
        .unwrap();
        let err = validate_engine_response("engine_forkchoiceUpdatedV3", &response).unwrap_err();
        assert!(err.to_string().contains("unknown ancestor"));
    }
}
