"""Run locally through docker exec. No public administrative endpoint."""

import sys
from app import SessionInput, broker


try:
    if sys.argv[1:] == ["revoke"]:
        broker.revoke()
        print("Review access disabled. Also revoke the Spotify app permissions.")
    elif not sys.argv[1:]:
        code = broker.provision(SessionInput.model_validate_json(sys.stdin.read(24576)))
        print("Review access enabled for 30 days. Paste this code only into Partner Center:")
        print(code)
    else:
        raise ValueError()
except Exception:
    # Do not print input, provider responses or validation errors with secrets.
    print("Could not configure review access. Check both saved sessions and Premium status.", file=sys.stderr)
    sys.exit(1)
