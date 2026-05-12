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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestMetrics {
    pub payload_count: u64,
    pub gas_used: u64,
}

pub fn parse_request_line(line: &str) -> Result<JsonRpcRequest> {
    serde_json::from_str(line).with_context(|| "parsing JSON-RPC request line")
}

pub fn request_metrics(request: &JsonRpcRequest) -> Result<RequestMetrics> {
    if !request.method.starts_with("engine_newPayload") {
        return Ok(RequestMetrics::default());
    }

    Ok(RequestMetrics {
        payload_count: 1,
        gas_used: new_payload_gas_used(request)?,
    })
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

fn new_payload_gas_used(request: &JsonRpcRequest) -> Result<u64> {
    let Some(payload) = request.params.as_array().and_then(|params| params.first()) else {
        return Ok(0);
    };
    let Some(gas_used) = payload.get("gasUsed") else {
        return Ok(0);
    };
    parse_quantity(gas_used).context("parsing engine_newPayload gasUsed")
}

fn parse_quantity(value: &Value) -> Result<u64> {
    if let Some(raw) = value.as_str() {
        let raw = raw.trim();
        if raw.is_empty() {
            anyhow::bail!("empty quantity");
        }
        if let Some(hex) = raw.strip_prefix("0x") {
            return u64::from_str_radix(hex, 16).context("parsing hex quantity");
        }
        return raw.parse::<u64>().context("parsing decimal quantity");
    }
    if let Some(number) = value.as_u64() {
        return Ok(number);
    }
    anyhow::bail!("quantity must be a string or unsigned integer")
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

    #[test]
    fn extracts_new_payload_gas_metrics() {
        let request = parse_request_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"engine_newPayloadV4","params":[{"gasUsed":"0xc8"}]}"#,
        )
        .unwrap();
        let metrics = request_metrics(&request).unwrap();
        assert_eq!(metrics.payload_count, 1);
        assert_eq!(metrics.gas_used, 200);

        let request = parse_request_line(
            r#"{"jsonrpc":"2.0","id":2,"method":"engine_forkchoiceUpdatedV3","params":[]}"#,
        )
        .unwrap();
        assert_eq!(
            request_metrics(&request).unwrap(),
            RequestMetrics::default()
        );
    }
}
