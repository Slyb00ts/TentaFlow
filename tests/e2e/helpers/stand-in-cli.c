/* =============================================================================
 * File: tests/e2e/helpers/stand-in-cli.c
 * Purpose: The smallest process that can stand in for a vendor coding-agent CLI
 *          on a machine that has no vendor CLI and no provider account.
 *
 * WHY C AND NOT A SCRIPT. The bridge runs every CLI inside the platform process
 * sandbox, whose read roots are `system_read_roots()` plus the resolved parents
 * of the executables it finds on PATH. An interpreter (`python3`, `node`) is
 * found on PATH and gets its own directory as a read root, but NOT its standard
 * library — so a script never reaches `main`. A natively compiled executable
 * has no such dependency: it is read from a directory that IS a read root.
 *
 * It is compiled by `stand-in-cli.js` and placed at the path the node's own
 * installation check reads (`<cache>/coding-agents/codex/<version>/bin/codex`),
 * which is also the first entry of the PATH Core hands the bridge. Nothing in
 * the product knows this program exists; it is reached only because it occupies
 * the name `codex` on that PATH.
 *
 * Three modes, and only the three the product actually invokes:
 *
 *   codex login --device-auth   The device-code sign-in. Prints a verification
 *                               URL, waits for one line on stdin, writes the
 *                               credential the provider would have written and
 *                               exits 0 — which is the bridge's success signal.
 *   codex login status          The authentication probe. Exits 0 only while
 *                               the credential it wrote is present and is a
 *                               non-empty JSON object.
 *   codex app-server            The JSON-RPC turn server: newline-delimited
 *                               frames on stdin/stdout, integer `id` correlation,
 *                               notifications without an `id`.
 *
 * WHAT IT DOES NOT DO. It never contacts a provider, never rotates a token and
 * never reports a plan or a subject the provider did not state. The credential
 * it writes is a well-formed JSON object and nothing more; anything the product
 * claims about the account beyond "a credential exists and the CLI exits 0" is
 * unproven by construction. See the spec's report section.
 * ========================================================================== */

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

/* The credential the vendor would have written. `tokens.account_id` is the
 * field the bridge's identity check reads, so an account signed in this way
 * carries a stable provider subject instead of an anonymous one. */
#define CREDENTIAL_JSON                                                        \
  "{\"tokens\":{\"account_id\":\"stand-in-account\","                          \
  "\"access_token\":\"stand-in-access-token\","                                \
  "\"refresh_token\":\"stand-in-refresh-token\"},"                             \
  "\"last_refresh\":\"2026-01-01T00:00:00Z\"}\n"

/* The verification URL. The node accepts the first `https://` address in the
 * terminal, and refuses one no longer than `https://x.y/`, so this is longer
 * than that origin on purpose and carries no trailing punctuation. */
#define VERIFICATION_URL "https://stand-in.invalid/device?code=STAND-IN-CODE\n"

/* The one place the credential path is decided: the engine's home directory.
 *
 * `CODEX_HOME` is REQUIRED, and there is deliberately no fallback to `$HOME`.
 * The bridge always sets it (`cli_command` applies the login home's environment
 * to every invocation), so a missing one means this program is being run by
 * something that is not the product. Falling back there would be the worst kind
 * of failure for a fixture: on a developer's machine `$HOME/.codex/auth.json` is
 * a REAL provider credential, and the sign-in mode OPENS THAT PATH FOR WRITING.
 * Refusing costs a loud error; guessing costs somebody their session. */
static int credential_path(char *out, size_t size) {
  const char *home = getenv("CODEX_HOME");
  if (home == NULL || home[0] == '\0') {
    fprintf(stderr, "stand-in-cli: CODEX_HOME is not set\n");
    return -1;
  }
  int written = snprintf(out, size, "%s/auth.json", home);
  return (written > 0 && (size_t)written < size) ? 0 : -1;
}

static int make_parent_directories(const char *path) {
  char buffer[4096];
  snprintf(buffer, sizeof(buffer), "%s", path);
  for (char *cursor = buffer + 1; *cursor != '\0'; cursor++) {
    if (*cursor != '/') continue;
    *cursor = '\0';
    if (mkdir(buffer, 0700) != 0 && errno != EEXIST) return -1;
    *cursor = '/';
  }
  return 0;
}

static int write_credential(void) {
  char path[4096];
  if (credential_path(path, sizeof(path)) != 0) return -1;
  if (make_parent_directories(path) != 0) return -1;
  FILE *file = fopen(path, "wx");
  if (file == NULL && errno == EEXIST) {
    /* A re-login replaces the account's credential, exactly as the vendor's
     * second sign-in would. */
    file = fopen(path, "w");
  }
  if (file == NULL) return -1;
  size_t length = strlen(CREDENTIAL_JSON);
  int ok = fwrite(CREDENTIAL_JSON, 1, length, file) == length && fflush(file) == 0;
  fclose(file);
  return ok ? 0 : -1;
}

/* Whether the file is a non-empty JSON object. The check is structural rather
 * than a parse: the only writer of this file is the mode above, so the question
 * the probe has to answer is "is there a credential here", not "is this
 * arbitrary JSON valid". */
static int credential_present(void) {
  char path[4096];
  if (credential_path(path, sizeof(path)) != 0) return 0;
  FILE *file = fopen(path, "rb");
  if (file == NULL) return 0;
  char buffer[8192];
  size_t length = fread(buffer, 1, sizeof(buffer) - 1, file);
  fclose(file);
  buffer[length] = '\0';
  size_t start = 0;
  while (start < length && (buffer[start] == ' ' || buffer[start] == '\n' ||
                            buffer[start] == '\t' || buffer[start] == '\r')) {
    start++;
  }
  size_t end = length;
  while (end > start && (buffer[end - 1] == ' ' || buffer[end - 1] == '\n' ||
                         buffer[end - 1] == '\t' || buffer[end - 1] == '\r')) {
    end--;
  }
  if (end <= start + 2 || buffer[start] != '{' || buffer[end - 1] != '}') return 0;
  for (size_t index = start + 1; index < end - 1; index++) {
    if (buffer[index] == '"') return 1;
  }
  return 0;
}

/* ---------------------------------------------------------------------------
 * codex app-server
 * ------------------------------------------------------------------------ */

/* The engine's own thread identity. It is a constant because this process owns
 * exactly one vendor thread; the product only requires that the id it is handed
 * on `thread/start` is the one it echoes back on `turn/start`. */
#define THREAD_ID "stand-in-thread-1"

/* The two lines a turn writes. The first is an assistant message delta, the
 * second the end of the turn with the status the product reads as success —
 * `codex_turn_state` accepts `turn/completed` with `turn.status=completed` and
 * nothing else as a completed turn. */
#define TURN_TEXT \
  "Zadanie z planu jest gotowe: notatki o zmianie sa w drzewie roboczym."

/* How long the turn takes to work before it reports anything.
 *
 * Two observations are only possible while the child is alive, and this is the
 * pause that makes both of them deterministic instead of a race.
 *
 * The bridge records this process in the account's `processes/` directory when
 * it starts it and DELETES that record when the child ends
 * (`coding-agent-bridge/src/process.rs`), so a turn that answers within
 * microseconds leaves a record a spec can only observe by luck. That alone
 * would need milliseconds.
 *
 * The console is the longer one: the account chip on a session's NOW-BAR is
 * painted from `loadRuns`, which `code-studio-session.js` runs on its own
 * cadence (`SIDE_POLL_TICKS`-th `TIMELINE_POLL_MS` tick, :134-135 — about ten
 * seconds), and the widget clears and hides the bar in its own `_render`
 * (`tf-agent-activity.js:507-514`) the moment no run is live. A turn shorter
 * than that interval ends before the label can be painted at all. A real CLI
 * turn lasts minutes, so outliving it is also the more faithful stand-in.
 *
 * Still far below every deadline on the path: the adapter's own turn timeout is
 * minutes and the bridge's RPC deadline for `turn/start` is 60 s. */
#define TURN_WORK_MS 20000

static void work_for(long milliseconds) {
  struct timespec pause = {
    .tv_sec = milliseconds / 1000,
    .tv_nsec = (milliseconds % 1000) * 1000000L,
  };
  while (nanosleep(&pause, &pause) != 0 && errno == EINTR) {
  }
}

static void respond(const char *id, const char *result) {
  printf("{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":%s}\n", id, result);
  fflush(stdout);
}

static void notify(const char *method, const char *params) {
  printf("{\"jsonrpc\":\"2.0\",\"method\":\"%s\",\"params\":%s}\n", method, params);
  fflush(stdout);
}

/* The request's `id` as it arrived, so the answer carries the same integer. An
 * id this reader cannot find means the frame is malformed for our purposes and
 * is dropped — answering with a guessed id would resolve somebody else's call. */
static int frame_id(const char *line, char *out, size_t size) {
  const char *key = strstr(line, "\"id\"");
  if (key == NULL) return -1;
  const char *colon = strchr(key, ':');
  if (colon == NULL) return -1;
  const char *value = colon + 1;
  while (*value == ' ' || *value == '\t') value++;
  size_t length = 0;
  while ((value[length] >= '0' && value[length] <= '9') ||
         (length == 0 && value[length] == '-')) {
    length++;
  }
  if (length == 0 || length >= size) return -1;
  memcpy(out, value, length);
  out[length] = '\0';
  return 0;
}

static int app_server(void) {
  char line[1 << 16];
  while (fgets(line, sizeof(line), stdin) != NULL) {
    const char *method_key = strstr(line, "\"method\"");
    if (method_key == NULL) continue;
    char id[32];
    int has_id = frame_id(line, id, sizeof(id)) == 0;
    /* A notification is never answered — an answer to one would be a frame the
     * bridge's reader has no pending request for, and the product's own
     * `initialized` notification is exactly this shape. */
    if (!has_id) continue;

    if (strstr(method_key, "\"initialize\"") != NULL) {
      respond(id, "{\"userAgent\":\"stand-in-codex/1.0\"}");
    } else if (strstr(method_key, "\"thread/start\"") != NULL ||
               strstr(method_key, "\"thread/resume\"") != NULL ||
               strstr(method_key, "\"thread/fork\"") != NULL) {
      char result[128];
      snprintf(result, sizeof(result), "{\"thread\":{\"id\":\"%s\"}}", THREAD_ID);
      respond(id, result);
    } else if (strstr(method_key, "\"turn/start\"") != NULL) {
      respond(id, "{\"turn\":{\"id\":\"stand-in-turn-1\",\"status\":\"in_progress\"}}");
      work_for(TURN_WORK_MS);
      notify("item/agentMessage/delta", "{\"delta\":\"" TURN_TEXT "\"}");
      notify("turn/completed", "{\"turn\":{\"status\":\"completed\"}}");
    } else if (strstr(method_key, "\"model/list\"") != NULL) {
      /* Model discovery runs on this same app-server. Answering it is what lets
       * the node's catalogue follow a real sign-in instead of staying empty. */
      respond(id,
              "{\"models\":[{\"id\":\"stand-in-model\",\"name\":\"Stand-in model\","
              "\"isDefault\":true}]}");
    } else {
      respond(id, "{}");
    }
  }
  return 0;
}

/* ---------------------------------------------------------------------------
 * Entry point
 * ------------------------------------------------------------------------ */

int main(int argc, char **argv) {
  if (argc < 2) {
    fprintf(stderr, "stand-in-cli: nothing to do\n");
    return 64;
  }
  if (strcmp(argv[1], "app-server") == 0) return app_server();
  if (strcmp(argv[1], "login") != 0) {
    fprintf(stderr, "stand-in-cli: unsupported command '%s'\n", argv[1]);
    return 64;
  }
  if (argc >= 3 && strcmp(argv[2], "status") == 0) {
    /* The vendor's own vocabulary: "Logged in" and an exit status of zero. */
    if (credential_present()) {
      printf("Logged in\n");
      return 0;
    }
    fprintf(stderr, "Not logged in\n");
    return 1;
  }

  /* Device-code sign-in. The URL is printed before anything is read, because
   * the node's `await_verification_url` returns as soon as it sees one and the
   * terminal must still be alive when the person types the code back. */
  fputs(VERIFICATION_URL, stdout);
  fflush(stdout);
  fputs("Enter the code shown in your browser: ", stdout);
  fflush(stdout);

  char code[512];
  if (fgets(code, sizeof(code), stdin) == NULL) {
    fprintf(stderr, "stand-in-cli: the sign-in was closed before a code arrived\n");
    return 1;
  }
  if (write_credential() != 0) {
    fprintf(stderr, "stand-in-cli: the credential could not be written\n");
    return 1;
  }
  fputs("Authentication complete.\n", stdout);
  fflush(stdout);
  return 0;
}
