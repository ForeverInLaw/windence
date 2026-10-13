import json
from pathlib import Path
import tempfile
import time
import unittest
from unittest.mock import patch

from fastapi.testclient import TestClient
import httpx

import app as api


class BrokerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.path = Path(self.directory.name) / "session.json"
        self.requests = []
        self.fail_playback = False

        def respond(request):
            self.requests.append(request)
            if request.url.path == "/v1/me":
                return httpx.Response(200, json={"id": "owner", "product": "premium"})
            playback = api.PLAYBACK_CLIENT_ID.encode() in request.content
            if playback and self.fail_playback:
                return httpx.Response(400, json={"error": "secret-upstream-details"})
            return httpx.Response(200, json={
                "access_token": "playback-access" if playback else "library-access",
                "refresh_token": "rotated-playback" if playback else "rotated-library",
                "expires_in": 3600, "scope": "streaming" if playback else "user-library-read",
            })

        self.http = httpx.Client(transport=httpx.MockTransport(respond))
        self.broker = api.Broker(self.path, self.http)
        self.patch = patch.object(api, "broker", self.broker)
        self.patch.start()
        self.client = TestClient(api.app, raise_server_exceptions=False)
        self.input = api.SessionInput(
            client_id="a" * 32, library_refresh_token="library-secret",
            playback_refresh_token="playback-secret",
        )

    def tearDown(self):
        self.client.close()
        self.patch.stop()
        self.http.close()
        self.directory.cleanup()

    def enable(self):
        return self.broker.provision(self.input)

    def session(self, code):
        return self.client.post("/v1/session", headers={"Authorization": "Bearer " + code})

    def test_access_is_disabled_until_provisioned(self):
        self.assertEqual(self.client.get("/healthz").status_code, 200)
        self.assertEqual(self.session("anything").status_code, 401)
        self.assertEqual(len(self.requests), 0)

    def test_tokens_are_short_lived_and_refresh_secrets_never_leave(self):
        code = self.enable()
        response = self.session(code)
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.headers["cache-control"], "no-store")
        data = response.json()
        self.assertEqual(data["account_id"], "owner")
        self.assertEqual(data["library"]["access_token"], "library-access")
        self.assertGreater(data["playback"]["expires_in"], 300)
        self.assertLessEqual(data["playback"]["expires_in"], 3600)
        self.assertNotIn("refresh_token", response.text)
        self.assertNotIn(code, self.path.read_text())
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(len(self.requests), 3, "Cached sessions must not renew again")

    def test_wrong_code_expiry_and_revocation_deny_access(self):
        code = self.enable()
        self.assertEqual(self.session(code + "x").status_code, 401)
        state = self.broker.read()
        state["valid_until"] = time.time() - 1
        self.broker.write(state)
        self.assertEqual(self.session(code).status_code, 401)
        code = self.enable()
        self.broker.revoke()
        self.assertEqual(self.session(code).status_code, 401)
        self.assertEqual(json.loads(self.path.read_text()), {})

    def test_rotated_refresh_token_survives_other_token_failure(self):
        code = self.enable()
        state = self.broker.read()
        for kind in ("library", "playback"):
            state[kind]["expires_at"] = 0
        self.broker.write(state)
        self.fail_playback = True
        response = self.session(code)
        self.assertEqual(response.status_code, 503)
        self.assertNotIn("secret-upstream-details", response.text)
        self.assertEqual(self.broker.read()["library"]["refresh_token"], "rotated-library")

    def test_failed_provisioning_does_not_enable_access(self):
        self.fail_playback = True
        with self.assertRaises(httpx.HTTPStatusError):
            self.enable()
        self.assertNotIn("code_hash", self.broker.read())
        self.assertEqual(self.session("anything").status_code, 401)


if __name__ == "__main__":
    unittest.main()
