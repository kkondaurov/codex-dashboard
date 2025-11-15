## Codex configuration:

Add to `~/.codex/config.toml`:

```
model_provider = "openai-via-proxy"

[model_providers.openai-via-proxy]
name = "OpenAI via proxy"
base_url = "http://127.0.0.1:8787/v1"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
```
