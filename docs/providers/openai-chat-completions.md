# OpenAI Chat Completions Provider

## Overview

The OpenAI Chat Completions provider implements support for OpenAI's standard Chat Completions API (`/v1/chat/completions`). This provider is compatible with any API that follows the OpenAI Chat Completions format, including:

- OpenAI (`https://api.openai.com`)
- Azure OpenAI
- Together AI
- DeepSeek
- Other OpenAI-compatible providers

## Features

### ✅ Implemented Features

1. **Message-Based Conversation**
   - Full support for system, user, assistant, and tool messages
   - Multi-turn conversation handling
   - Message history management

2. **Tool Calling**
   - OpenAI function calling format
   - Multiple tool calls in single response
   - Tool result handling

3. **Streaming Responses** (Not Yet Implemented)
   - Streaming infrastructure exists but not wired into provider
   - Currently uses non-streaming requests
   - Future enhancement planned

4. **Error Handling**
   - Comprehensive error classification
   - Rate limiting detection
   - Authentication error handling
   - Context length validation
   - Server error retry logic

5. **Token Usage Tracking**
   - Input token counting
   - Output token counting
   - Prompt cache usage detection
   - Cost estimation support

### 🔄 Continuation Support

**Note**: Incremental continuation is currently disabled for Chat Completions provider. The provider sends full requests on each turn. This design choice ensures reliability given the message-based format of the Chat Completions API.

Future enhancements may enable safe continuation mechanisms.

## Configuration

### Basic Setup

```json
{
  "providers": {
    "openai": {
      "base_url": "https://api.openai.com",
      "credential": {
        "source": "environment",
        "env": "OPENAI_API_KEY"
      }
    }
  },
  "models": [
    {
      "model_ref": "openai/gpt-4o-mini",
      "provider_id": "openai",
      "name": "gpt-4o-mini"
    },
    {
      "model_ref": "openai/gpt-4o",
      "provider_id": "openai",
      "name": "gpt-4o"
    }
  ]
}
```

### Alternative Provider Example (Together AI)

```json
{
  "providers": {
    "together": {
      "base_url": "https://api.together.xyz",
      "credential": {
        "source": "environment",
        "env": "TOGETHER_API_KEY"
      }
    }
  }
}
```

## Usage

### Configuration in holon.json

```json
{
  "model": "openai/gpt-4o-mini",
  "providers": {
    "openai": {
      "base_url": "https://api.openai.com",
      "credential": {
        "source": "environment",
        "env": "OPENAI_API_KEY"
      }
    }
  }
}
```

### Environment Variables

- `HOLON_LIVE_CHAT_COMPLETION_MODEL`: Override model for live testing
- `OPENAI_API_KEY`: Your OpenAI API key
- `HOLON_LIVE_OPENAI_MODEL`: Generic OpenAI model override

## API Compatibility

### Supported Features

| Feature | Support | Notes |
|---------|---------|-------|
| Text completions | ✅ | Full support |
| Tool calling | ✅ | Function calling format |
| Streaming | ⚠️ | Infrastructure exists but not enabled |
| Multi-turn | ✅ | Conversation history |
| Parallel tools | ⚠️ | `tool_choice: "auto"` (single) |
| Incremental continuation | ❌ | Disabled for reliability |
| Prompt caching | ✅ | `prompt_cache_key` tracking |
| Schema strict mode | ✅ | Respects `ToolSchemaContract` |

### Provider Compatibility

| Provider | Compatible | Notes |
|----------|------------|-------|
| OpenAI | ✅ | Full compatibility |
| Together AI | ✅ | API-compatible |
| DeepSeek | ✅ | API-compatible |
| Azure OpenAI | ❌ | Not currently supported |
| Other OpenAI-compatible | ✅ | Works with standard `/v1/chat/completions` format |

**Note on Azure OpenAI**: This provider is not compatible with Azure OpenAI because:
- Different authentication (API keys in headers vs Bearer tokens)
- Different API paths and versioning requirements
- Different request/response formats

Future implementation of Azure-specific provider would be needed for Azure OpenAI support.

### Request Format

The provider sends requests in the standard Chat Completions format:

```json
{
  "model": "gpt-4o-mini",
  "messages": [
    {"role": "system", "content": "You are a helpful assistant."},
    {"role": "user", "content": "Hello!"}
  ],
  "max_tokens": 4096,
  "stream": false,
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "calculator",
        "description": "A simple calculator",
        "parameters": {
          "type": "object",
          "properties": {
            "expression": {"type": "string"}
          },
          "required": ["expression"]
        },
        "strict": false
      }
    }
  ],
  "tool_choice": "auto"
}
```

### Response Handling

The provider properly handles:

1. **Content extraction**: Text responses from assistant messages
2. **Tool call parsing**: Function calling format with arguments
3. **Finish reasons**: Proper stop_reason mapping
4. **Usage tracking**: Token counts from `usage` field
5. **Error responses**: OpenAI error format with type/code/message

## Testing

### Unit Tests

```bash
cargo test --lib chat_completion
```

### Integration Tests

```bash
# Requires real API credentials
cargo test --test live_openai_chat_completions
```

### Test Coverage

- ✅ Message conversion (text, tools, multi-turn)
- ✅ Request building (tools, tool_choice)
- ✅ Response parsing (content, tool_calls, usage)
- ✅ Streaming infrastructure (not currently enabled in production)
- ✅ Error classification (rate limits, auth, context length)
- ✅ Edge cases (empty inputs, special characters, large payloads)
- ✅ Live API tests (ignored without credentials)

## Performance Characteristics

### Request Processing

- **Non-streaming requests**: Current implementation uses standard blocking requests
- **Response buffering**: Full response received before processing
- **Token counting**: Accurate token usage reporting from API

### Safety Limits

- **Timeout**: 60 seconds default HTTP timeout
- **Token limits**: Configurable via `max_tokens`
- **Response size**: Maximum output tokens enforced by model

### Token Usage

Chat Completions API provides detailed token usage:

```json
{
  "usage": {
    "prompt_tokens": 20,
    "completion_tokens": 10,
    "total_tokens": 30,
    "prompt_tokens_details": {
      "cached_tokens": 5
    }
  }
}
```

The provider tracks:
- Input tokens: `prompt_tokens`
- Output tokens: `completion_tokens`
- Cache reads: `cached_tokens` (when available)

## Error Recovery

### Retryable Errors

- **Rate limited** (`rate_limit_exceeded`): Automatic retry with exponential backoff
- **Server errors** (5xx): Retry with backoff

### Fail-Fast Errors

- **Authentication** (`invalid_api_key`): Immediate failure
- **Context length** (`context_length_exceeded`): Requires input reduction
- **Invalid requests**: Contract validation errors

### Monitoring

Check provider diagnostics:

```rust
if let Some(diagnostics) = response.request_diagnostics {
    println!("Request mode: {}", diagnostics.request_lowering_mode);
    if let Some(continuation) = diagnostics.incremental_continuation {
        println!("Continuation status: {}", continuation.status);
    }
}
```

## Compatibility Notes

### Together AI

```json
{
  "providers": {
    "together": {
      "base_url": "https://api.together.xyz",
      "credential": {
        "source": "environment",
        "env": "TOGETHER_API_KEY"
      }
    }
  }
}
```

Model references: `together/mixtral-8x7b-instruct-v0-1`

### DeepSeek

```json
{
  "providers": {
    "deepseek": {
      "base_url": "https://api.deepseek.com",
      "credential": {
        "source": "environment",
        "env": "DEEPSEEK_API_KEY"
      }
    }
  }
}
```

Model references: `deepseek/deepseek-chat`

## Troubleshooting

### Common Issues

1. **"missing configured credential"**
   - Ensure `OPENAI_API_KEY` or provider-specific env var is set
   - Check provider credential configuration

2. **"context_length_exceeded"**
   - Reduce input message length
   - Increase model's context limit (if available)
   - Enable prompt caching to reduce effective input size

3. **"rate_limit_exceeded"**
   - Implement client-side rate limiting
   - Add retry delays between requests
   - Consider higher-tier API plan

4. **Streaming timeout**
   - Increase HTTP timeout if needed
   - Check network connectivity
   - Verify server load/status

### Debug Mode

Enable verbose logging:

```bash
RUST_LOG=debug holon run "your prompt"
```

## Future Enhancements

Potential improvements under consideration:

1. **Incremental continuation**: Safe message prefix matching
2. **Advanced streaming**: Chunked response handling
3. **Batch processing**: Multiple requests in single API call
4. **Semantic caching**: Response caching for repeated queries
5. **Cost optimization**: Model selection based on complexity

## Contributing

When extending the Chat Completions provider:

1. **Maintain compatibility**: Ensure OpenAI format compliance
2. **Add tests**: Unit tests for new features
3. **Update docs**: Document new capabilities
4. **Error handling**: Proper classification and recovery
5. **Streaming first**: Consider streaming implications

## References

- [OpenAI Chat Completions API](https://platform.openai.com/docs/api-reference/chat/create)
- [Function Calling](https://platform.openai.com/docs/guides/function-calling)
- [Streaming Responses](https://platform.openai.com/docs/api-reference/chat/streaming)
- [Error Codes](https://platform.openai.com/docs/guides/error-codes)

## Changelog

### Initial Implementation (PR #543)
- ✅ Basic Chat Completions support
- ✅ Tool calling (function format)
- ⚠️ Streaming infrastructure (not enabled in production)
- ✅ Error classification
- ✅ Comprehensive testing
- ✅ Documentation

### Additional Features (Current PR)
- ✅ Real API integration tests
- ✅ Edge case coverage
- ✅ Performance validation
- ✅ Enhanced documentation
