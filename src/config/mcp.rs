use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub enabled: bool,
    pub address: String,
    pub transport: String, // "stdio" or "http"
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            enabled: false,
            address: "127.0.0.1:8000".to_string(),
            transport: "http".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct PartialMcpServerConfig {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub address: Option<String>,
    pub transport: Option<String>,
}
