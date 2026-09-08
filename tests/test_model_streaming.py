import asyncio

import pytest

pytest.importorskip("pydantic_ai")
from pydantic_ai.models.function import FunctionModel
from pydantic_ai.models.test import TestModel

from anchor.runtime.capabilities import ModelProfile
from anchor.runtime.model_gateway import PydanticAIModelGateway, ToolFunction, build_model_gateway


class Secrets:
    def get(self, name):
        return "offline-test-key"


@pytest.mark.parametrize("provider", ["zenmux", "a6api", "deepseek"])
@pytest.mark.parametrize("wire_api", ["responses", "chat_completions"])
def test_named_openai_compatible_provider_is_supported(provider, wire_api):
    client = build_model_gateway(ModelProfile(
        ref=provider, provider=provider, model="test", base_url="https://example.test/v1",
        wire_api=wire_api, secret_ref="unused"), Secrets())
    assert isinstance(client, PydanticAIModelGateway)
    asyncio.run(client.close())


def gateway(model):
    return PydanticAIModelGateway(ModelProfile(ref="stream", provider="test", model="test",
                                  secret_ref="unused", stream=True), Secrets(), model=model)


def test_openai_sdk_retries_are_disabled_in_favor_of_durable_node_retries():
    client = gateway(TestModel())
    try:
        assert client._openai_client.max_retries == 0
        assert client._http_client.timeout.read == 900.0
    finally:
        asyncio.run(client.close())


def test_streaming_returns_complete_output_after_real_tool_loop():
    async def run():
        client = gateway(TestModel())
        calls = []
        async def tool(arguments):
            calls.append(arguments)
            return "Retrieved evidence"
        try:
            result = await client.generate_with_tools(prompt="Research", tools=[ToolFunction(name="search", call=tool)])
            assert calls
            assert "Retrieved evidence" in result.text
        finally:
            await client.close()
    asyncio.run(run())


def test_broken_stream_does_not_return_partial_result():
    async def broken_stream(messages, info):
        yield "Partial manuscript"
        raise RuntimeError("connection lost")
    async def run():
        client = gateway(FunctionModel(stream_function=broken_stream))
        try:
            with pytest.raises(RuntimeError, match="connection lost"):
                await client.generate(prompt="Research")
        finally:
            await client.close()
    asyncio.run(run())
