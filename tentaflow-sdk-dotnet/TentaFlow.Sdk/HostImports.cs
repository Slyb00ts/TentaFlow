// ===== File: HostImports.cs — raw wasm imports from module "tentaflow" =====
// One-to-one bindings for the host functions registered in
// tentaflow-core/src/addon/host_functions/mod.rs. All pointers are guest
// linear-memory offsets passed as i32; [WasmImportLinkage] makes the DllImport
// module name the wasm import module.

#nullable enable

using System.Runtime.InteropServices;

namespace TentaFlow.Sdk;

internal static partial class HostImports
{
    private const string Module = "tentaflow";

    // --- Log ---

    [DllImport(Module, EntryPoint = "log_info")]
    [WasmImportLinkage]
    internal static extern int LogInfo(int msgPtr, int msgLen);

    [DllImport(Module, EntryPoint = "log_warn")]
    [WasmImportLinkage]
    internal static extern int LogWarn(int msgPtr, int msgLen);

    [DllImport(Module, EntryPoint = "log_error")]
    [WasmImportLinkage]
    internal static extern int LogError(int msgPtr, int msgLen);

    // --- Storage (key/value) ---

    [DllImport(Module, EntryPoint = "storage_get")]
    [WasmImportLinkage]
    internal static extern int StorageGet(
        int keyPtr, int keyLen, int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "storage_set")]
    [WasmImportLinkage]
    internal static extern int StorageSet(int keyPtr, int keyLen, int valPtr, int valLen);

    [DllImport(Module, EntryPoint = "storage_delete")]
    [WasmImportLinkage]
    internal static extern int StorageDelete(int keyPtr, int keyLen);

    [DllImport(Module, EntryPoint = "storage_list")]
    [WasmImportLinkage]
    internal static extern int StorageList(
        int prefixPtr, int prefixLen, int outPtr, int outCap, int outLenPtr);

    // --- Shared state (host-side AddonStateStore) ---

    [DllImport(Module, EntryPoint = "state_get_v1")]
    [WasmImportLinkage]
    internal static extern int StateGetV1(
        int keyPtr, int keyLen, int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "state_set_v1")]
    [WasmImportLinkage]
    internal static extern int StateSetV1(int inPtr, int inLen);

    [DllImport(Module, EntryPoint = "state_delete_v1")]
    [WasmImportLinkage]
    internal static extern int StateDeleteV1(int keyPtr, int keyLen);

    [DllImport(Module, EntryPoint = "state_list_v1")]
    [WasmImportLinkage]
    internal static extern int StateListV1(
        int prefixPtr, int prefixLen, int outPtr, int outCap, int outLenPtr);

    // --- Config (install-time connection params) ---

    [DllImport(Module, EntryPoint = "config_get_v1")]
    [WasmImportLinkage]
    internal static extern int ConfigGetV1(
        int keyPtr, int keyLen, int outPtr, int outCap, int outLenPtr);

    // --- Secrets ---

    [DllImport(Module, EntryPoint = "secret_get")]
    [WasmImportLinkage]
    internal static extern int SecretGet(
        int keyPtr, int keyLen, int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "secret_set")]
    [WasmImportLinkage]
    internal static extern int SecretSet(int keyPtr, int keyLen, int valPtr, int valLen);

    // --- HTTP ---

    [DllImport(Module, EntryPoint = "http_request")]
    [WasmImportLinkage]
    internal static extern int HttpRequest(
        int reqPtr, int reqLen, int outPtr, int outCap, int outLenPtr);

    // --- LLM ---

    [DllImport(Module, EntryPoint = "llm_generate")]
    [WasmImportLinkage]
    internal static extern int LlmGenerate(
        int promptPtr, int promptLen,
        int modelPtr, int modelLen,
        int optionsPtr, int optionsLen,
        int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "llm_generate_stream_start")]
    [WasmImportLinkage]
    internal static extern int LlmGenerateStreamStart(
        int promptPtr, int promptLen,
        int modelPtr, int modelLen,
        int optionsPtr, int optionsLen);

    // Wire: CBOR LlmStreamNextInput {callback_id, timeout_ms} → LlmStreamNextOutput
    // {chunks, finished, finish_reason?, error?}. The callback_id is carried inside
    // the CBOR input, not as a bare argument (matches the host stt/service ABI).
    [DllImport(Module, EntryPoint = "llm_generate_stream_next")]
    [WasmImportLinkage]
    internal static extern int LlmGenerateStreamNext(
        int inPtr, int inLen, int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "llm_generate_stream_cancel")]
    [WasmImportLinkage]
    internal static extern int LlmGenerateStreamCancel(int callbackId);

    // --- STT (speech-to-text) ---

    [DllImport(Module, EntryPoint = "stt_transcribe_v1")]
    [WasmImportLinkage]
    internal static extern int SttTranscribeV1(
        int inPtr, int inLen, int outPtr, int outCap, int outLenPtr);

    // --- Document / blob store (per-instance file store) ---

    // document_get_v1 writes chunk bytes to blob_out and the DocumentGetMeta CBOR
    // to meta_out (meta_out_len holds the CBOR length; the chunk length lives in
    // the meta as `chunk_len`). Retries on a too-small blob buffer are surfaced via
    // AbiError::OutputBufferTooSmall with the required size in meta_out_len.
    [DllImport(Module, EntryPoint = "document_get_v1")]
    [WasmImportLinkage]
    internal static extern int DocumentGetV1(
        int inPtr, int inLen,
        int blobOutPtr, int blobOutCap,
        int metaOutPtr, int metaOutCap, int metaOutLenPtr);

    // --- Events ---

    [DllImport(Module, EntryPoint = "event_publish")]
    [WasmImportLinkage]
    internal static extern int EventPublish(
        int eventTypePtr, int eventTypeLen, int payloadJsonPtr, int payloadJsonLen);

    [DllImport(Module, EntryPoint = "event_subscribe")]
    [WasmImportLinkage]
    internal static extern int EventSubscribe(
        int eventTypePtr, int eventTypeLen, int filterJsonPtr, int filterJsonLen);

    // --- UI ---

    [DllImport(Module, EntryPoint = "ui_render_cbor")]
    [WasmImportLinkage]
    internal static extern int UiRenderCbor(int cborPtr, int cborLen);

    [DllImport(Module, EntryPoint = "ui_notify")]
    [WasmImportLinkage]
    internal static extern int UiNotify(
        int titlePtr, int titleLen, int bodyPtr, int bodyLen, int levelPtr, int levelLen);

    // --- User ---

    [DllImport(Module, EntryPoint = "user_get_current")]
    [WasmImportLinkage]
    internal static extern int UserGetCurrent(int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "user_check_permission")]
    [WasmImportLinkage]
    internal static extern int UserCheckPermission(
        int permissionTypePtr, int permissionTypeLen,
        int resourcePtr, int resourceLen,
        int accessLevelPtr, int accessLevelLen);

    // --- Directory (org users / groups / roles) ---
    // Output-only ABI: CBOR Directory*Output shapes from tentaflow-sdk-spec.
    // All four require the "directory.read" permission.

    [DllImport(Module, EntryPoint = "directory_users_v1")]
    [WasmImportLinkage]
    internal static extern int DirectoryUsersV1(int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "directory_groups_v1")]
    [WasmImportLinkage]
    internal static extern int DirectoryGroupsV1(int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "directory_roles_v1")]
    [WasmImportLinkage]
    internal static extern int DirectoryRolesV1(int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "directory_org_v1")]
    [WasmImportLinkage]
    internal static extern int DirectoryOrgV1(int outPtr, int outCap, int outLenPtr);

    // --- Model aliases (readonly; requires the "alias.read" permission) ---

    // No input; writes a JSON `{ "aliases": [AvailableAlias...] }` document — the
    // aliases/models this addon may consume (its [[uses_alias]] grants).
    [DllImport(Module, EntryPoint = "alias_list_available_v1")]
    [WasmImportLinkage]
    internal static extern int AliasListAvailableV1(int outPtr, int outCap, int outLenPtr);

    // --- SQL (per-addon SQLite) ---

    [DllImport(Module, EntryPoint = "sql_exec_v1")]
    [WasmImportLinkage]
    internal static extern int SqlExecV1(
        int queryPtr, int queryLen,
        int paramsJsonPtr, int paramsJsonLen,
        int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "sql_query_v1")]
    [WasmImportLinkage]
    internal static extern int SqlQueryV1(
        int queryPtr, int queryLen,
        int paramsJsonPtr, int paramsJsonLen,
        int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "sql_query_one_v1")]
    [WasmImportLinkage]
    internal static extern int SqlQueryOneV1(
        int queryPtr, int queryLen,
        int paramsJsonPtr, int paramsJsonLen,
        int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "sql_transaction_v1")]
    [WasmImportLinkage]
    internal static extern int SqlTransactionV1(
        int statementsJsonPtr, int statementsJsonLen,
        int outPtr, int outCap, int outLenPtr);

    // --- Flow ---

    [DllImport(Module, EntryPoint = "flow_invoke_v1")]
    [WasmImportLinkage]
    internal static extern int FlowInvokeV1(
        int inputPtr, int inputLen, int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "flow_status_v1")]
    [WasmImportLinkage]
    internal static extern int FlowStatusV1(
        int inputPtr, int inputLen, int outPtr, int outCap, int outLenPtr);

    [DllImport(Module, EntryPoint = "flow_cancel_v1")]
    [WasmImportLinkage]
    internal static extern int FlowCancelV1(
        int inputPtr, int inputLen, int outPtr, int outCap, int outLenPtr);

    // --- Services (QUIC proxy to registered services) ---

    [DllImport(Module, EntryPoint = "service_request")]
    [WasmImportLinkage]
    internal static extern int ServiceRequest(
        int servicePtr, int serviceLen,
        int requestPtr, int requestLen,
        int outPtr, int outCap, int outLenPtr);

    // --- Tools (LLM tool calling registration) ---

    [DllImport(Module, EntryPoint = "tool_register")]
    [WasmImportLinkage]
    internal static extern int ToolRegister(int defPtr, int defLen);
}
