"""Temporary certification access. Refresh tokens never leave this service."""

from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import secrets
import tempfile
import time
from typing import Annotated

from fastapi import FastAPI, Header, HTTPException
from fastapi.responses import JSONResponse
import httpx
from pydantic import BaseModel, ConfigDict, Field, SecretStr


PLAYBACK_CLIENT_ID = "65b708073fc0480ea92a077233ca87bd"
TOKEN_URL = "https://accounts.spotify.com/api/token"


class SessionInput(BaseModel):
    model_config = ConfigDict(extra="forbid")
    client_id: str = Field(pattern=r"^[0-9a-fA-F]{32}$")
    library_refresh_token: SecretStr = Field(min_length=1, max_length=8192)
    playback_refresh_token: SecretStr = Field(min_length=1, max_length=8192)


class SpotifyToken(BaseModel):
    access_token: SecretStr = Field(min_length=1)
    refresh_token: SecretStr | None = None
    expires_in: int = Field(ge=120, le=3600)
    scope: str = ""


class AccessDenied(Exception):
    pass


class UpstreamUnavailable(Exception):
    pass


class Broker:
    def __init__(self, path: Path, http: httpx.Client | None = None):
        self.path = path
        self.http = http or httpx.Client(timeout=15, trust_env=False, follow_redirects=False)

    @contextmanager
    def locked(self):
        # Provisioning, revocation and renewal share this lock across processes.
        with self.path.with_suffix(".lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)

    def read(self) -> dict:
        try:
            return json.loads(self.path.read_text())
        except FileNotFoundError:
            return {}

    def write(self, state: dict):
        descriptor, temporary = tempfile.mkstemp(dir=self.path.parent)
        try:
            with os.fdopen(descriptor, "w") as file:
                json.dump(state, file)
                file.flush()
                os.fsync(file.fileno())
            os.replace(temporary, self.path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    def refresh(self, state: dict, kind: str):
        credential = state[kind]
        if credential.get("expires_at", 0) > time.time() + 90:
            return
        started = time.time()
        response = self.http.post(TOKEN_URL, data={
            "grant_type": "refresh_token",
            "refresh_token": credential["refresh_token"],
            "client_id": state["client_id"] if kind == "library" else PLAYBACK_CLIENT_ID,
        })
        response.raise_for_status()
        token = SpotifyToken.model_validate(response.json())
        credential.update({
            "access_token": token.access_token.get_secret_value(),
            "expires_at": started + token.expires_in,
            "scope": token.scope,
        })
        if token.refresh_token:
            credential["refresh_token"] = token.refresh_token.get_secret_value()
        # Save a rotated token even if renewal of the other credential fails.
        self.write(state)

    def session(self, authorization: str) -> dict:
        with self.locked():
            state = self.read()
            supplied = hashlib.sha256(authorization.encode()).hexdigest()
            if not secrets.compare_digest(supplied, state.get("code_hash", "")):
                raise AccessDenied()
            if time.time() >= state.get("valid_until", 0):
                raise AccessDenied()
            try:
                for kind in ("library", "playback"):
                    self.refresh(state, kind)
            except (httpx.HTTPError, ValueError, KeyError):
                # Upstream errors can contain credentials. Return no details.
                raise UpstreamUnavailable() from None
            if time.time() >= state["valid_until"]:
                raise AccessDenied()
            return {
                "client_id": state["client_id"],
                "account_id": state["account_id"],
                **{kind: {
                    "access_token": state[kind]["access_token"],
                    "expires_in": int(state[kind]["expires_at"] - time.time()),
                    "scope": state[kind]["scope"],
                } for kind in ("library", "playback")},
            }

    def provision(self, incoming: SessionInput) -> str:
        code = "review-" + secrets.token_hex(32)
        with self.locked():
            state = {
                "client_id": incoming.client_id,
                "library": {"refresh_token": incoming.library_refresh_token.get_secret_value()},
                "playback": {"refresh_token": incoming.playback_refresh_token.get_secret_value()},
            }
            # Do not enable access until both credentials have been renewed.
            self.write(state)
            for kind in ("library", "playback"):
                self.refresh(state, kind)
            response = self.http.get("https://api.spotify.com/v1/me", headers={
                "Authorization": "Bearer " + state["library"]["access_token"],
            })
            response.raise_for_status()
            if response.json().get("product") != "premium":
                raise ValueError("A Spotify Premium account is required")
            state["account_id"] = response.json()["id"]
            state["code_hash"] = hashlib.sha256(("Bearer " + code).encode()).hexdigest()
            state["valid_until"] = time.time() + 30 * 86400
            self.write(state)
        return code

    def revoke(self):
        with self.locked():
            self.write({})


broker = Broker(Path(os.environ.get("STATE_PATH", "/data/session.json")))
app = FastAPI(docs_url=None, redoc_url=None, openapi_url=None)


@app.get("/healthz")
def health():
    return {"status": "ok"}


@app.post("/v1/session")
def session(authorization: Annotated[str, Header()] = ""):
    try:
        return JSONResponse(broker.session(authorization), headers={"Cache-Control": "no-store"})
    except AccessDenied:
        raise HTTPException(401, "Review access is unavailable or expired") from None
    except UpstreamUnavailable:
        raise HTTPException(503, "Spotify is unavailable; try again later") from None
