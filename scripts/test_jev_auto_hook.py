"""Exercise the shipped Jev observer with synthetic payloads and no network."""

import asyncio
import copy
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import types
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / "src-tauri/src/tool_manager.rs"
RUST = SOURCE.read_text(encoding="utf-8")
INJECTION = RUST.split('const SITECUSTOMIZE_PY: &str = r#"', 1)[1].split('\n"#;', 1)[0]
HOOK = textwrap.dedent(
    INJECTION.split("    # Jev auto observer:", 1)[1]
    .split("    # End Jev auto observer.", 1)[0]
    .split("\n", 1)[1]
)


def user(text="review this function"):
    return {"role": "user", "content": [{"type": "input_text", "text": text}]}


def payload(text="review this function", session="session-a"):
    return {"model": "test-model", "prompt_cache_key": session, "input": [user(text)]}


class ObserverTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.config = Path(self.temp.name) / "client-setup.json"
        self.set_enabled(True)
        self.calls = []
        self.original_calls = []
        self.original_error = None
        self.helper_error = None
        self.helper_called = threading.Event()

        test = self

        class OpenAIHandlerMixin:
            async def _compress_openai_responses_payload_in_executor(self, body, **kwargs):
                test.original_calls.append((body, kwargs))
                if test.original_error:
                    raise test.original_error
                return body, "original-result", kwargs

        self.handler_type = OpenAIHandlerMixin
        self.original_method = OpenAIHandlerMixin._compress_openai_responses_payload_in_executor
        modules = {}
        for name in ("headroom", "headroom.proxy", "headroom.proxy.handlers"):
            module = types.ModuleType(name)
            module.__path__ = []
            modules[name] = module
        openai = types.ModuleType("headroom.proxy.handlers.openai")
        openai.OpenAIHandlerMixin = OpenAIHandlerMixin
        modules[openai.__name__] = openai
        modules["headroom"].proxy = modules["headroom.proxy"]
        modules["headroom.proxy"].handlers = modules["headroom.proxy.handlers"]
        modules["headroom.proxy.handlers"].openai = openai
        self.modules = modules

        self.env = {
            "HEADROOM_JEV_EXECUTABLE": str(Path(self.temp.name) / "app-helper"),
            "HEADROOM_JEV_CONFIG": str(self.config),
        }
        self.install()

    def set_enabled(self, enabled):
        self.config.write_text(json.dumps({"jevAutoEnabled": enabled}), encoding="utf-8")

    def fake_run(self, command, **kwargs):
        self.calls.append((command, kwargs))
        self.helper_called.set()
        if self.helper_error:
            raise self.helper_error
        return types.SimpleNamespace(returncode=0)

    def install(self, env=None):
        self.handler_type._compress_openai_responses_payload_in_executor = self.original_method
        namespace = {"_hd_os": os}
        effective_env = dict(self.env if env is None else env)
        with patch.dict(os.environ, effective_env, clear=True), patch.dict(sys.modules, self.modules):
            exec(compile(HOOK, "jev_auto_observer", "exec"), namespace)
        if "_hd_ja_subprocess" in namespace:
            namespace["_hd_ja_subprocess"] = types.SimpleNamespace(
                run=self.fake_run, DEVNULL=subprocess.DEVNULL
            )
        self.ns = namespace
        self.handler = self.handler_type()

    def evaluate(self, body, **kwargs):
        kwargs.setdefault("client", "codex")
        kwargs.setdefault("request_id", "http-request")
        return asyncio.run(
            self.handler._compress_openai_responses_payload_in_executor(body, **kwargs)
        )

    def drain(self):
        jobs = self.ns["_hd_ja_queue"]
        deadline = time.monotonic() + 2
        while jobs.unfinished_tasks and time.monotonic() < deadline:
            time.sleep(0.005)
        self.assertEqual(jobs.unfinished_tasks, 0, "observer worker did not finish")

    def test_whole_injection_is_valid_python(self):
        compile(INJECTION, "sitecustomize.py", "exec")

    def test_http_and_websocket_share_observer_without_mutating_request(self):
        body = payload()
        body["input"] = [
            {"role": "system", "content": "system-only"},
            {"role": "developer", "content": "developer-only"},
            user("old user text"),
            {"role": "assistant", "content": "assistant-only"},
            {"type": "function_call_output", "output": "tool-output-only"},
            {"role": "user", "content": [
                {"type": "input_text", "text": "first user part"},
                {"type": "input_image", "image_url": "image-only"},
                {"type": "text", "text": "second user part"},
                {"type": "tool_result", "text": "nested-tool-only"},
            ]},
        ]
        original = copy.deepcopy(body)
        result = self.evaluate(body, savings_tags={"session_id": "http-session"})
        self.assertIs(result[0], body)
        self.assertEqual(result[1], "original-result")
        self.assertEqual(body, original)

        websocket = copy.deepcopy(body)
        websocket["prompt_cache_key"] = "ws-session"
        ws_result = self.evaluate(websocket, timeout=2.5, request_id="ws-request")
        self.assertIs(ws_result[0], websocket)
        self.assertEqual(ws_result[2]["timeout"], 2.5)
        self.drain()
        self.assertEqual(len(self.calls), 2)
        for command, kwargs in self.calls:
            self.assertEqual(command, [self.env["HEADROOM_JEV_EXECUTABLE"], "--jev-auto-classify"])
            job = json.loads(kwargs["input"])
            self.assertEqual(job["text"], "first user part\nsecond user part")
            self.assertEqual(len(job["dedupKey"]), 64)
            self.assertEqual(kwargs["stdout"], subprocess.DEVNULL)
            self.assertEqual(kwargs["stderr"], subprocess.DEVNULL)
            self.assertLessEqual(kwargs["timeout"], 20)
        self.assertTrue(self.ns["_hd_ja_worker_thread"].daemon)

    def test_missing_environment_leaves_original_method_installed(self):
        self.install({"HEADROOM_JEV_CONFIG": str(self.config)})
        self.assertIs(
            self.handler_type._compress_openai_responses_payload_in_executor,
            self.original_method,
        )
        self.evaluate(payload())
        self.assertEqual(self.calls, [])

    def test_disabled_missing_and_malformed_config_skip_work(self):
        for enabled in (False, "true", 1):
            self.set_enabled(enabled)
            self.evaluate(payload())
        self.config.unlink()
        self.evaluate(payload())
        self.config.write_text("invalid-json", encoding="utf-8")
        self.evaluate(payload())
        self.assertEqual(self.calls, [])
        self.assertIsNone(self.ns["_hd_ja_worker_thread"])

    def test_non_codex_missing_text_images_and_oversize_are_skipped(self):
        self.evaluate(payload(), client="opencode")
        self.evaluate(payload(), client="claude-code")
        for body in (
            {}, {"input": "implicit input is not an explicit user message"},
            {"input": [{"role": "assistant", "content": "assistant only"}]},
            {"input": [user(" ")]},
            {"input": [user(), {"role": "user", "content": [{"type": "input_image"}]}]},
            payload("x" * (16 * 1024 + 1)),
            payload("中" * 6000),
        ):
            self.evaluate(body)
        self.assertEqual(self.calls, [])
        self.assertIsNone(self.ns["_hd_ja_worker_thread"])
        self.evaluate(payload("x" * (16 * 1024)))
        self.drain()
        self.assertEqual(len(json.loads(self.calls[0][1]["input"])["text"]), 16 * 1024)

    def test_tool_turns_deduplicate_within_session_and_user_position(self):
        body = payload()
        self.evaluate(body)
        tool_turn = copy.deepcopy(body)
        tool_turn["input"].append({"type": "function_call_output", "output": "synthetic tool result"})
        self.evaluate(tool_turn, request_id="new-http-request")
        self.evaluate(payload(session="session-b"))
        next_turn = copy.deepcopy(tool_turn)
        next_turn["input"].append(user())
        self.evaluate(next_turn)
        self.drain()
        self.assertEqual(len(self.calls), 3)
        self.assertEqual(len({json.loads(call[1]["input"])["dedupKey"] for call in self.calls}), 3)

    def test_savings_tags_supply_session_identity_when_cache_key_missing(self):
        body = payload()
        del body["prompt_cache_key"]
        self.evaluate(body, request_id="one", savings_tags={"thread_id": "same-thread"})
        self.evaluate(body, request_id="two", savings_tags={"thread_id": "same-thread"})
        self.evaluate(body, request_id="three", savings_tags={"thread_id": "other-thread"})
        self.drain()
        self.assertEqual(len(self.calls), 2)

    def test_queue_full_drops_observation_and_recent_keys_are_bounded(self):
        self.assertEqual(self.ns["_hd_ja_queue"].maxsize, 8)
        self.ns["_hd_ja_worker_thread"] = types.SimpleNamespace(is_alive=lambda: True)
        for index in range(8):
            self.ns["_hd_ja_queue"].put_nowait({"dedupKey": str(index)})
        result = self.evaluate(payload())
        self.assertEqual(result[1], "original-result")
        self.assertEqual(len(self.ns["_hd_ja_recent"]), 0)
        self.assertEqual(self.calls, [])
        self.ns["_hd_ja_queue"] = queue.Queue(maxsize=8)
        for index in range(300):
            self.ns["_hd_ja_submit"]({"text": "synthetic", "dedupKey": str(index)})
            self.ns["_hd_ja_queue"].get_nowait()
            self.ns["_hd_ja_queue"].task_done()
        self.assertEqual(len(self.ns["_hd_ja_recent"]), 256)

    def test_worker_failure_does_not_change_original_result_or_stop_worker(self):
        self.helper_error = subprocess.TimeoutExpired("fake-helper", 18)
        first = self.evaluate(payload("first"))
        self.drain()
        self.helper_error = OSError("synthetic launch failure")
        second = self.evaluate(payload("second"))
        self.drain()
        self.helper_error = None
        third = self.evaluate(payload("third"))
        self.drain()
        self.assertEqual([result[1] for result in (first, second, third)], ["original-result"] * 3)
        self.assertEqual(len(self.calls), 3)
        self.assertTrue(self.ns["_hd_ja_worker_thread"].is_alive())

    def test_slow_worker_does_not_block_and_off_discards_queued_work(self):
        started = threading.Event()
        release = threading.Event()
        self.addCleanup(release.set)

        def slow_run(command, **kwargs):
            started.set()
            release.wait(2)
            return self.fake_run(command, **kwargs)

        self.ns["_hd_ja_subprocess"].run = slow_run
        before = time.monotonic()
        self.evaluate(payload("first"))
        self.assertLess(time.monotonic() - before, 0.5)
        self.assertTrue(started.wait(1))
        self.evaluate(payload("queued"))
        self.set_enabled(False)
        release.set()
        self.drain()
        self.assertEqual(len(self.calls), 1)

    def test_observer_exceptions_do_not_mask_original_return_or_exception(self):
        def fail(_job):
            raise RuntimeError("observer-only failure")

        self.ns["_hd_ja_submit"] = fail
        self.assertEqual(self.evaluate(payload())[1], "original-result")
        self.original_error = RuntimeError("original-compressor failure")
        with self.assertRaises(RuntimeError) as raised:
            self.evaluate(payload())
        self.assertIs(raised.exception, self.original_error)


if __name__ == "__main__":
    unittest.main()
