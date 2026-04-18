/**
 * Widok zdekodowanego envelope'u wystawiony do JS. Body wyciete jako osobny
 * Uint8Array zeby call-site mogl zdekodowac MessageBody osobno.
 */
export class EnvelopeView {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(EnvelopeView.prototype);
        obj.__wbg_ptr = ptr;
        EnvelopeViewFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        EnvelopeViewFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_envelopeview_free(ptr, 0);
    }
    /**
     * Rkyv-zakodowany MessageBody — przekazac do `decodeMessageBody()`.
     * @returns {Uint8Array}
     */
    get body() {
        const ret = wasm.envelopeview_body(this.__wbg_ptr);
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * True jesli flaga `IS_ERROR` ustawiona (body = `MessageBody::Error`).
     * @returns {boolean}
     */
    get isError() {
        const ret = wasm.envelopeview_isError(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * True jesli flaga `IS_STREAM_CHUNK` ustawiona.
     * @returns {boolean}
     */
    get isStreamChunk() {
        const ret = wasm.envelopeview_isStreamChunk(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * True jesli flaga `IS_STREAM_END` ustawiona.
     * @returns {boolean}
     */
    get isStreamEnd() {
        const ret = wasm.envelopeview_isStreamEnd(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * 32-byte target node id jesli Routing::Forward, inaczej None.
     * @returns {Uint8Array | undefined}
     */
    get targetNodeId() {
        const ret = wasm.envelopeview_targetNodeId(this.__wbg_ptr);
        let v1;
        if (ret[0] !== 0) {
            v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
            wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        }
        return v1;
    }
    /**
     * @returns {bigint}
     */
    get correlation_id() {
        const ret = wasm.__wbg_get_envelopeview_correlation_id(this.__wbg_ptr);
        return BigInt.asUintN(64, ret);
    }
    /**
     * @returns {number}
     */
    get flags() {
        const ret = wasm.__wbg_get_envelopeview_flags(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {boolean}
     */
    get is_forward() {
        const ret = wasm.__wbg_get_envelopeview_is_forward(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * @returns {number}
     */
    get message_kind() {
        const ret = wasm.__wbg_get_envelopeview_message_kind(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {number}
     */
    get schema_version() {
        const ret = wasm.__wbg_get_envelopeview_schema_version(this.__wbg_ptr);
        return ret;
    }
    /**
     * @returns {bigint}
     */
    get sequence() {
        const ret = wasm.__wbg_get_envelopeview_sequence(this.__wbg_ptr);
        return BigInt.asUintN(64, ret);
    }
}
if (Symbol.dispose) EnvelopeView.prototype[Symbol.dispose] = EnvelopeView.prototype.free;

/**
 * Wersja schematu protokolu. MUSI byc zgodna ze `tentaflow_protocol::SCHEMA_VERSION`
 * po stronie serwera — handshake sprawdza match, mismatch = reject connection.
 * @returns {number}
 */
export function SCHEMA_VERSION() {
    const ret = wasm.SCHEMA_VERSION();
    return ret;
}

/**
 * Decode + bytecheck (NIGDY `access_unchecked`) pelnego envelope'u z WSS input.
 * Zwraca strukturalny widok; body wciaz zakodowany (lazy decode przez
 * `decodeMessageBody`).
 * @param {Uint8Array} bytes
 * @returns {EnvelopeView}
 */
export function decodeEnvelope(bytes) {
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.decodeEnvelope(ptr0, len0);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return EnvelopeView.__wrap(ret[0]);
}

/**
 * Dekoduje rkyv-zakodowany MessageBody na JS object w formacie
 * `{ variant: "NodeListResponse", nodes: [...] }`. Dla bootstrap variantow
 * pokrywa 10 kejsow; nieznany variant zwraca `{ variant: "Unknown" }`.
 * @param {Uint8Array} bytes
 * @returns {any}
 */
export function decodeMessageBody(bytes) {
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.decodeMessageBody(ptr0, len0);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return takeFromExternrefTable0(ret[0]);
}

/**
 * MessageBody::ApiKeyCreateRequest { name, scopes }.
 * @param {string} name
 * @param {string[]} scopes
 * @returns {Uint8Array}
 */
export function encodeApiKeyCreateRequest(name, scopes) {
    const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArrayJsValueToWasm0(scopes, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeApiKeyCreateRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::ApiKeyListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeApiKeyListRequest() {
    const ret = wasm.encodeApiKeyListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::ApiKeyRevokeRequest { key_id }.
 * @param {string} key_id
 * @returns {Uint8Array}
 */
export function encodeApiKeyRevokeRequest(key_id) {
    const ptr0 = passStringToWasm0(key_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeApiKeyRevokeRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::AuthLoginRequest { username, password }.
 * @param {string} username
 * @param {string} password
 * @returns {Uint8Array}
 */
export function encodeAuthLoginRequest(username, password) {
    const ptr0 = passStringToWasm0(username, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAuthLoginRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::AuthMeRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeAuthMeRequest() {
    const ret = wasm.encodeAuthMeRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::ChatStreamRequest — przyjmuje JSON string messages, parsuje
 * jako JsValue. Bootstrap accepts tylko `model_id` + jednoelementowa lista
 * user messages. Pelny messages[] input po integracji serde-wasm-bindgen (#36 ph.2).
 * @param {string} model_id
 * @param {string} user_message
 * @returns {Uint8Array}
 */
export function encodeChatStreamRequestSimple(model_id, user_message) {
    const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(user_message, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeChatStreamRequestSimple(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::ClusterUpdateRequest.
 * @param {string} cluster_id
 * @param {string} name
 * @param {string | null} [description]
 * @returns {Uint8Array}
 */
export function encodeClusterUpdateRequest(cluster_id, name, description) {
    const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterUpdateRequest(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * MessageBody::DashboardMetricsRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeDashboardMetricsRequest() {
    const ret = wasm.encodeDashboardMetricsRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * Buduje Envelope (routing=Direct) z podanymi polami + body bytes; zwraca
 * rkyv-zakodowany frame jako Uint8Array.
 *
 * `correlation_id` przekazywany jako u64 (BigInt po stronie JS).
 * @param {bigint} correlation_id
 * @param {bigint} sequence
 * @param {number} message_kind
 * @param {Uint8Array} body
 * @returns {Uint8Array}
 */
export function encodeEnvelopeDirect(correlation_id, sequence, message_kind, body) {
    const ptr0 = passArray8ToWasm0(body, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeEnvelopeDirect(correlation_id, sequence, message_kind, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::MeshPairInitRequest { node_id (32 bytes), pin }.
 * @param {Uint8Array} node_id
 * @param {string} pin
 * @returns {Uint8Array}
 */
export function encodeMeshPairInitRequest(node_id, pin) {
    const ptr0 = passArray8ToWasm0(node_id, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(pin, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshPairInitRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::MeshPeersListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeMeshPeersListRequest() {
    const ret = wasm.encodeMeshPeersListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::MetaCancelStream (unit variant). Correlation_id idzie w envelope.
 * @returns {Uint8Array}
 */
export function encodeMetaCancelStream() {
    const ret = wasm.encodeMetaCancelStream();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::MetaHeartbeat { sent_at_epoch }.
 * @param {bigint} sent_at_epoch
 * @returns {Uint8Array}
 */
export function encodeMetaHeartbeat(sent_at_epoch) {
    const ret = wasm.encodeMetaHeartbeat(sent_at_epoch);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::MetaSchemaVersionCheck { client_version }.
 * Wysylane raz przy handshake — jesli serwer odrzuci, disconnect.
 * @param {number} client_version
 * @returns {Uint8Array}
 */
export function encodeMetaSchemaVersionCheck(client_version) {
    const ret = wasm.encodeMetaSchemaVersionCheck(client_version);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::ModelListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeModelListRequest() {
    const ret = wasm.encodeModelListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::NodeInfoRequest { node_id }. node_id MUSI byc 32 bajtami.
 * @param {Uint8Array} node_id
 * @returns {Uint8Array}
 */
export function encodeNodeInfoRequest(node_id) {
    const ptr0 = passArray8ToWasm0(node_id, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeNodeInfoRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::NodeListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeNodeListRequest() {
    const ret = wasm.encodeNodeListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::SettingsListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeSettingsListRequest() {
    const ret = wasm.encodeSettingsListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::SettingsUpdateRequest — simplified: para key/value/is_secret.
 * Pelna lista (N elementow) po integracji serde-wasm-bindgen (#36 phase 2).
 * @param {string} key
 * @param {string} value
 * @param {boolean} is_secret
 * @returns {Uint8Array}
 */
export function encodeSettingsUpdateSingle(key, value, is_secret) {
    const ptr0 = passStringToWasm0(key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(value, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeSettingsUpdateSingle(ptr0, len0, ptr1, len1, is_secret);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::SubscribeResumeRequest { resume_token }.
 * Klient po reconnect przekazuje token z poprzedniej SubscribeResumeOffer.
 * @param {Uint8Array} resume_token
 * @returns {Uint8Array}
 */
export function encodeSubscribeResumeRequest(resume_token) {
    const ptr0 = passArray8ToWasm0(resume_token, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeSubscribeResumeRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * Stale discriminantow message_kind dla dispatchu po stronie JS.
 * Wolac `messageKind()` raz, cachowac result.
 * @returns {any}
 */
export function messageKind() {
    const ret = wasm.messageKind();
    return ret;
}

/**
 * Szybka walidacja ze bajty maja prawidlowy ksztalt (pelny bytecheck envelope)
 * bez zwracania widoku. Uzyte do wczesnego odrzucenia malformed frames przed
 * enqueue do dispatch queue.
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
export function validateFrame(bytes) {
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.validateFrame(ptr0, len0);
    return ret !== 0;
}

/**
 * Inicjalizacja modulu — ustawia panic hook dla lepszych bledow w console.
 * Wolane raz po zaladowaniu .wasm w przegladarce.
 */
export function wasm_main() {
    wasm.wasm_main();
}

function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_8c4e43fe74559d73: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg___wbindgen_string_get_72fb696202c56729: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_be289d5034ed271b: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_error_7534b8e9a36f1ab4: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_new_361308b2356cecd0: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_3eb36ae241fe6f44: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_8a6f238a6ece86ea: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_from_slice_a3d2629dc1826784: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_push_8ffdcb2063340ba5: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_set_6cb8631f80447a67: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_stack_0ed75d68575b0f3c: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbindgen_cast_0000000000000001: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0) {
            // Cast intrinsic for `U64 -> Externref`.
            const ret = BigInt.asUintN(64, arg0);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./wasm_glue_bg.js": import0,
    };
}

const EnvelopeViewFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_envelopeview_free(ptr >>> 0, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayJsValueToWasm0(array, malloc) {
    const ptr = malloc(array.length * 4, 4) >>> 0;
    for (let i = 0; i < array.length; i++) {
        const add = addToExternrefTable0(array[i]);
        getDataViewMemory0().setUint32(ptr + 4 * i, add, true);
    }
    WASM_VECTOR_LEN = array.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('wasm_glue_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
