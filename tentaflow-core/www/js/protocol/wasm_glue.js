/**
 * Widok zdekodowanego envelope'u wystawiony do JS. Body wyciete jako osobny
 * Uint8Array zeby call-site mogl zdekodowac MessageBody osobno.
 */
export class EnvelopeView {
    static __wrap(ptr) {
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
     * CBOR-zakodowany MessageBody — przekazac do `decodeMessageBody()`.
     * @returns {Uint8Array}
     */
    get body() {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.envelopeview_body(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            var v1 = getArrayU8FromWasm0(r0, r1).slice();
            wasm.__wbindgen_export3(r0, r1 * 1, 1);
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
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
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            wasm.envelopeview_targetNodeId(retptr, this.__wbg_ptr);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            let v1;
            if (r0 !== 0) {
                v1 = getArrayU8FromWasm0(r0, r1).slice();
                wasm.__wbindgen_export3(r0, r1 * 1, 1);
            }
            return v1;
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
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
 * Zwraca hex Ed25519 public key (64 znaki). Generuje keypair przy pierwszym
 * uzyciu i persistuje w localStorage.
 * @returns {string}
 */
export function browserNodeId() {
    let deferred2_0;
    let deferred2_1;
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.browserNodeId(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        var ptr1 = r0;
        var len1 = r1;
        if (r3) {
            ptr1 = 0; len1 = 0;
            throw takeObject(r2);
        }
        deferred2_0 = ptr1;
        deferred2_1 = len1;
        return getStringFromWasm0(ptr1, len1);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
        wasm.__wbindgen_export3(deferred2_0, deferred2_1, 1);
    }
}

/**
 * Usuwa keypair z localStorage (wylogowanie/reset tozsamosci browser).
 * Kolejne wywolanie `browserNodeId` wygeneruje nowy keypair.
 */
export function browserResetIdentity() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.browserResetIdentity(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        if (r1) {
            throw takeObject(r0);
        }
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Podpisuje `data` i zwraca raw bajty podpisu (64 B).
 * @param {Uint8Array} data
 * @returns {Uint8Array}
 */
export function browserSign(data) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.browserSign(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Podpisuje `data` kluczem prywatnym browser-a. Zwraca signature (64 bajty)
 * jako hex string (128 znakow).
 * @param {Uint8Array} data
 * @returns {string}
 */
export function browserSignHex(data) {
    let deferred3_0;
    let deferred3_1;
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.browserSignHex(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        var ptr2 = r0;
        var len2 = r1;
        if (r3) {
            ptr2 = 0; len2 = 0;
            throw takeObject(r2);
        }
        deferred3_0 = ptr2;
        deferred3_1 = len2;
        return getStringFromWasm0(ptr2, len2);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
        wasm.__wbindgen_export3(deferred3_0, deferred3_1, 1);
    }
}

/**
 * Decode a CBOR-encoded Component into a JS object suitable for ComponentRenderer.
 * @param {Uint8Array} cbor_bytes
 * @returns {any}
 */
export function decodeComponentCbor(cbor_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(cbor_bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.decodeComponentCbor(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        if (r2) {
            throw takeObject(r1);
        }
        return takeObject(r0);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Decode + bytecheck (NIGDY `access_unchecked`) pelnego envelope'u z WSS input.
 * Zwraca strukturalny widok; body wciaz zakodowany (lazy decode przez
 * `decodeMessageBody`).
 * @param {Uint8Array} bytes
 * @returns {EnvelopeView}
 */
export function decodeEnvelope(bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.decodeEnvelope(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        if (r2) {
            throw takeObject(r1);
        }
        return EnvelopeView.__wrap(r0);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Decode the RAW canonical LiDAR frame bytes pushed in a `StreamFrame.data`
 * (L3a real-time PUSH stream `streamId = "lidar:<robot_id>"`) into the JS frame
 * projection. Reuses the sdk-spec header layout via `lidar_frame_to_js`; a
 * malformed/short frame returns `{hasFrame: false}` (no panic).
 * @param {Uint8Array} bytes
 * @returns {any}
 */
export function decodeLidarFrame(bytes) {
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.decodeLidarFrame(ptr0, len0);
    return takeObject(ret);
}

/**
 * Dekoduje CBOR-zakodowany MessageBody na JS object.
 * Dla znanych variantow zwraca obiekt z polem `variant`, a dla nieznanego
 * variantu `{ variant: "Unknown" }`.
 * @param {Uint8Array} bytes
 * @returns {any}
 */
export function decodeMessageBody(bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.decodeMessageBody(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        if (r2) {
            throw takeObject(r1);
        }
        return takeObject(r0);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Decode CBOR-encoded Vec<PatchOp> into JS array of { path, op, ... }.
 * @param {Uint8Array} cbor_bytes
 * @returns {any}
 */
export function decodePatchOpsCbor(cbor_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(cbor_bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.decodePatchOpsCbor(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        if (r2) {
            throw takeObject(r1);
        }
        return takeObject(r0);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Decode CBOR-encoded Vec<StateEntry> into JS array of { path, value }.
 * @param {Uint8Array} cbor_bytes
 * @returns {any}
 */
export function decodeStateEntriesCbor(cbor_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(cbor_bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.decodeStateEntriesCbor(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        if (r2) {
            throw takeObject(r1);
        }
        return takeObject(r0);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Decode UI channel CBOR payload into a JS-friendly object.
 * @param {Uint8Array} cbor_bytes
 * @returns {any}
 */
export function decodeUiPayload(cbor_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(cbor_bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.decodeUiPayload(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        if (r2) {
            throw takeObject(r1);
        }
        return takeObject(r0);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonAccessDecisionRequest { addon_id, kind, target, decision }.
 * @param {string} addon_id
 * @param {string} kind
 * @param {string} target
 * @param {string} decision
 * @returns {Uint8Array}
 */
export function encodeAddonAccessDecisionRequest(addon_id, kind, target, decision) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(target, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(decision, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeAddonAccessDecisionRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonAccessListRequest { addon_id }.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonAccessListRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonAccessListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonAdminOnlySetRequest { addon_id, admin_only }.
 * @param {string} addon_id
 * @param {boolean} admin_only
 * @returns {Uint8Array}
 */
export function encodeAddonAdminOnlySetRequest(addon_id, admin_only) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonAdminOnlySetRequest(retptr, ptr0, len0, admin_only);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonInstanceBody(ReqCatalogList) — lista pakietow w katalogu.
 * @returns {Uint8Array}
 */
export function encodeAddonCatalogListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAddonCatalogListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonConfigGetRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonConfigGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `keys` + `values` — rownolegle wektory (len(keys) == len(values)); laczymy po indeksie.
 * wasm-bindgen nie wspiera `Vec<(String,String)>` bezposrednio, a `Vec<String>` dziala.
 * @param {string} addon_id
 * @param {string[]} keys
 * @param {string[]} values
 * @returns {Uint8Array}
 */
export function encodeAddonConfigSetRequest(addon_id, keys, values) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(keys, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayJsValueToWasm0(values, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeAddonConfigSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonDetailRequest { addon_id } — szczegoly addona.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonDetailRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonDocumentUploadChunkRequestBody — jeden fragment pliku
 * wgrywanego z panelu UI addona do jego document store. `org_id` NIE jest tu —
 * serwer bierze org z sesji. `bytes` to surowy fragment (Uint8Array).
 * @param {string} addon_id
 * @param {string} upload_id
 * @param {string} filename
 * @param {string} mime
 * @param {number} seq
 * @param {number} total_chunks
 * @param {string} source
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeAddonDocumentUploadChunkRequest(addon_id, upload_id, filename, mime, seq, total_chunks, source, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(mime, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(source, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len5 = WASM_VECTOR_LEN;
        wasm.encodeAddonDocumentUploadChunkRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, seq, total_chunks, ptr4, len4, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} filename
 * @param {Uint8Array} content
 * @returns {Uint8Array}
 */
export function encodeAddonInstallRequest(filename, content) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(content, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAddonInstallRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonInstanceBody(ReqDuplicate) — duplikacja instancji.
 * @param {string} source_addon_id
 * @param {string} new_display_name
 * @returns {Uint8Array}
 */
export function encodeAddonInstanceDuplicateRequest(source_addon_id, new_display_name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(source_addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(new_display_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAddonInstanceDuplicateRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonInstanceBody(ReqInstall) — instalacja instancji z katalogu.
 * `config` is a JS `Array<[key, value]>` of install-time connection-param
 * values (e.g. the robot IP). Empty for non-robot packages.
 * @param {string} package_id
 * @param {string} version
 * @param {string} display_name
 * @param {any} config
 * @returns {Uint8Array}
 */
export function encodeAddonInstanceInstallRequest(package_id, version, display_name, config) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(package_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(version, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(display_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeAddonInstanceInstallRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, addHeapObject(config));
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonInstanceBody(ReqUpdate) — hot-update instancji do wersji.
 * @param {string} addon_id
 * @param {string} target_version
 * @returns {Uint8Array}
 */
export function encodeAddonInstanceUpdateRequest(addon_id, target_version) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_version, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAddonInstanceUpdateRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonInstanceBody(ReqVersions) — wersje dostepne dla instancji.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonInstanceVersionsRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonInstanceVersionsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @param {number} limit
 * @param {number} offset
 * @param {string | null} [level]
 * @param {string | null} [search]
 * @returns {Uint8Array}
 */
export function encodeAddonLogsRequest(addon_id, limit, offset, level, search) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(level) ? 0 : passStringToWasm0(level, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(search) ? 0 : passStringToWasm0(search, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeAddonLogsRequest(retptr, ptr0, len0, limit, offset, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonNetworkRulesGetRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonNetworkRulesGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @param {string[]} allowed_hosts
 * @param {string[]} blocked_hosts
 * @param {string} mode
 * @returns {Uint8Array}
 */
export function encodeAddonNetworkRulesSetRequest(addon_id, allowed_hosts, blocked_hosts, mode) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(allowed_hosts, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayJsValueToWasm0(blocked_hosts, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeAddonNetworkRulesSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthAuthorizeStartRequest — inicjuje flow autoryzacji.
 * @param {string} addon_id
 * @param {string} provider_id
 * @param {string} mode
 * @param {string | null} [redirect_after]
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthAuthorizeStartRequest(addon_id, provider_id, mode, redirect_after) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(redirect_after) ? 0 : passStringToWasm0(redirect_after, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeAddonOAuthAuthorizeStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthConfigClearSecretRequest — usun wylacznie secret.
 * @param {string} addon_id
 * @param {string} provider_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthConfigClearSecretRequest(addon_id, provider_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAddonOAuthConfigClearSecretRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthConfigListRequest { addon_id } — zero secretow.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthConfigListRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonOAuthConfigListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthConfigSetRequest — zapis konfiguracji OAuth.
 * `client_secret` = None (null) => zachowaj obecny, Some(..) => nadpisz.
 * @param {string} addon_id
 * @param {string} provider_id
 * @param {string} client_id
 * @param {string | null | undefined} client_secret
 * @param {string} redirect_uri
 * @param {boolean} enabled
 * @param {string} oauth_mode
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthConfigSetRequest(addon_id, provider_id, client_id, client_secret, redirect_uri, enabled, oauth_mode) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(client_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(client_secret) ? 0 : passStringToWasm0(client_secret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(redirect_uri, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passStringToWasm0(oauth_mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len5 = WASM_VECTOR_LEN;
        wasm.encodeAddonOAuthConfigSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, enabled, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthLinkedAccountsRequest — lista polaczonych kont.
 * `scope` = "all" (admin) lub "mine" (user).
 * @param {string} addon_id
 * @param {string} scope
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthLinkedAccountsRequest(addon_id, scope) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(scope, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAddonOAuthLinkedAccountsRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthReauthorizeRequest { account_id }.
 * @param {number} account_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthReauthorizeRequest(account_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAddonOAuthReauthorizeRequest(retptr, account_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthRevokeRequest { account_id }.
 * @param {number} account_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthRevokeRequest(account_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAddonOAuthRevokeRequest(retptr, account_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonOAuthTestConnectionRequest { addon_id, provider_id }.
 * @param {string} addon_id
 * @param {string} provider_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthTestConnectionRequest(addon_id, provider_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAddonOAuthTestConnectionRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonPermissionCatalogRequest { addon_id } — katalog deklaracji.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionCatalogRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonPermissionCatalogRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonPermissionCheckRequest — czy uzytkownik ma uprawnienie.
 * `user_id` = None (pass null z JS) => serwer uzyje id z sesji.
 * @param {string} addon_id
 * @param {string} permission_id
 * @param {string | null} [user_id]
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionCheckRequest(addon_id, permission_id, user_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(permission_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(user_id) ? 0 : passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeAddonPermissionCheckRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonPermissionDefaultSetRequest — ustawia domyslny grant addona.
 * @param {string} addon_id
 * @param {string} permission_id
 * @param {string} grant_mode
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionDefaultSetRequest(addon_id, permission_id, grant_mode) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(permission_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(grant_mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeAddonPermissionDefaultSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonPermissionMatrixRequest { addon_id } — aktualna macierz.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionMatrixRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonPermissionMatrixRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonPermissionSetRequest — ustawia grant per (user|group).
 * @param {string} addon_id
 * @param {string} subject_type
 * @param {string} subject_id
 * @param {string} permission_id
 * @param {string} grant_mode
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionSetRequest(addon_id, subject_type, subject_id, permission_id, grant_mode) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(permission_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(grant_mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeAddonPermissionSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonReloadRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonReloadRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonResourcesGetRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonResourcesGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @param {number} max_instances
 * @param {number} cpu_limit_pct
 * @param {number} ram_mb
 * @param {number} storage_mb
 * @param {number} http_requests_per_min
 * @param {number} llm_tokens_per_min
 * @returns {Uint8Array}
 */
export function encodeAddonResourcesSetRequest(addon_id, max_instances, cpu_limit_pct, ram_mb, storage_mb, http_requests_per_min, llm_tokens_per_min) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonResourcesSetRequest(retptr, ptr0, len0, max_instances, cpu_limit_pct, ram_mb, storage_mb, http_requests_per_min, llm_tokens_per_min);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonShowInCatalogSetRequest { addon_id, show_in_catalog }.
 * @param {string} addon_id
 * @param {boolean} show_in_catalog
 * @returns {Uint8Array}
 */
export function encodeAddonShowInCatalogSetRequest(addon_id, show_in_catalog) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonShowInCatalogSetRequest(retptr, ptr0, len0, show_in_catalog);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonStorageBody(StatsRequest) — statystyki storage addona.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonStorageStatsRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonStorageStatsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonTeardownPlanRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonTeardownPlanRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @param {boolean} enabled
 * @returns {Uint8Array}
 */
export function encodeAddonToggleRequest(addon_id, enabled) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonToggleRequest(retptr, ptr0, len0, enabled);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonToolsRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonToolsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonUninstallRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonUninstallRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonVectorBody(GetConfigRequest) — config vector backendu addona.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonVectorGetConfigRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonVectorGetConfigRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonVectorBody(SetConfigRequest) — zapis config vector backendu.
 * Pola configu jako osobne argumenty (bez serde_json w crate wasm).
 * @param {string} addon_id
 * @param {string} backend
 * @param {string | null} [milvus_source]
 * @param {string | null} [service_node_id]
 * @param {string | null} [service_id]
 * @param {string | null} [manual_uri]
 * @param {string | null} [collection_override]
 * @param {string | null} [milvus_user]
 * @param {string | null} [milvus_password]
 * @returns {Uint8Array}
 */
export function encodeAddonVectorSetConfigRequest(addon_id, backend, milvus_source, service_node_id, service_id, manual_uri, collection_override, milvus_user, milvus_password) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(backend, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(milvus_source) ? 0 : passStringToWasm0(milvus_source, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(service_node_id) ? 0 : passStringToWasm0(service_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(service_id) ? 0 : passStringToWasm0(service_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(manual_uri) ? 0 : passStringToWasm0(manual_uri, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        var ptr6 = isLikeNone(collection_override) ? 0 : passStringToWasm0(collection_override, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len6 = WASM_VECTOR_LEN;
        var ptr7 = isLikeNone(milvus_user) ? 0 : passStringToWasm0(milvus_user, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len7 = WASM_VECTOR_LEN;
        var ptr8 = isLikeNone(milvus_password) ? 0 : passStringToWasm0(milvus_password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len8 = WASM_VECTOR_LEN;
        wasm.encodeAddonVectorSetConfigRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6, ptr7, len7, ptr8, len8);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v10 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v10;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonVisibilityListRequest { addon_id } — widocznosc per grupa.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonVisibilityListRequest(addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAddonVisibilityListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonVisibilitySetRequest { addon_id, group_id, visible }.
 * @param {string} addon_id
 * @param {string} group_id
 * @param {boolean} visible
 * @returns {Uint8Array}
 */
export function encodeAddonVisibilitySetRequest(addon_id, group_id, visible) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(group_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAddonVisibilitySetRequest(retptr, ptr0, len0, ptr1, len1, visible);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonsListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeAddonsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAddonsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} messages_json
 * @returns {Uint8Array}
 */
export function encodeAgentBuilderAssistRequest(messages_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(messages_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAgentBuilderAssistRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @param {string} request_id
 * @param {string} decision
 * @returns {Uint8Array}
 */
export function encodeAgentPermissionReplyRequest(run_id, request_id, decision) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(request_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(decision, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeAgentPermissionReplyRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeAgentRunCancelRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAgentRunCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeAgentRunDetailRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAgentRunDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Subscribe to a run-events scope. `scope_kind` is "session" or "run";
 * `scope_id` is the session id or run id respectively.
 * @param {string} scope_kind
 * @param {string} scope_id
 * @returns {Uint8Array}
 */
export function encodeAgentRunEventsSubscribeRequest(scope_kind, scope_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(scope_kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(scope_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAgentRunEventsSubscribeRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @param {string} question_id
 * @param {string} answer
 * @returns {Uint8Array}
 */
export function encodeAgentRunReplyRequest(run_id, question_id, answer) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(question_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(answer, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeAgentRunReplyRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} agent_id
 * @param {string} prompt
 * @returns {Uint8Array}
 */
export function encodeAgentRunStartRequest(agent_id, prompt) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(agent_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(prompt, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAgentRunStartRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string | null} [agent_id]
 * @param {string | null} [status]
 * @param {string | null} [parent_run_id]
 * @returns {Uint8Array}
 */
export function encodeAgentRunsListRequest(agent_id, status, parent_run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(agent_id) ? 0 : passStringToWasm0(agent_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(status) ? 0 : passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(parent_run_id) ? 0 : passStringToWasm0(parent_run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeAgentRunsListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} agent_id
 * @returns {Uint8Array}
 */
export function encodeAgentsDeleteRequest(agent_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(agent_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAgentsDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} agent_id
 * @returns {Uint8Array}
 */
export function encodeAgentsDetailRequest(agent_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(agent_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAgentsDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {boolean | null} [enabled]
 * @param {boolean | null} [routable]
 * @returns {Uint8Array}
 */
export function encodeAgentsListRequest(enabled, routable) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAgentsListRequest(retptr, isLikeNone(enabled) ? 0xFFFFFF : enabled ? 1 : 0, isLikeNone(routable) ? 0xFFFFFF : routable ? 1 : 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} agent_json
 * @returns {Uint8Array}
 */
export function encodeAgentsUpsertRequest(agent_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(agent_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAgentsUpsertRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AliasConsumerGrantRequest { alias_id, addon_id }.
 * @param {number} alias_id
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAliasConsumerGrantRequest(alias_id, addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAliasConsumerGrantRequest(retptr, alias_id, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AliasConsumerListRequest { alias_id }.
 * @param {number} alias_id
 * @returns {Uint8Array}
 */
export function encodeAliasConsumerListRequest(alias_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAliasConsumerListRequest(retptr, alias_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AliasConsumerRevokeRequest { alias_id, addon_id }.
 * @param {number} alias_id
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAliasConsumerRevokeRequest(alias_id, addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAliasConsumerRevokeRequest(retptr, alias_id, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AliasVisibilitySetRequest { alias_id, visibility }.
 * @param {number} alias_id
 * @param {string} visibility
 * @returns {Uint8Array}
 */
export function encodeAliasVisibilitySetRequest(alias_id, visibility) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(visibility, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeAliasVisibilitySetRequest(retptr, alias_id, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ApiKeyCreateRequest { name, key_type, subject_id, scope_resources }.
 * `scope_resources` travels as two parallel arrays (types[i] + ids[i]) so the
 * wasm-bindgen boundary stays on simple `Vec<String>` values.
 * @param {string} name
 * @param {string} key_type
 * @param {string | null | undefined} subject_id
 * @param {string[]} scope_types
 * @param {string[]} scope_ids
 * @returns {Uint8Array}
 */
export function encodeApiKeyCreateRequest(name, key_type, subject_id, scope_types, scope_ids) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(key_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(subject_id) ? 0 : passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        const ptr3 = passArrayJsValueToWasm0(scope_types, wasm.__wbindgen_export);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passArrayJsValueToWasm0(scope_ids, wasm.__wbindgen_export);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeApiKeyCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ApiKeyListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeApiKeyListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeApiKeyListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ApiKeyRevokeRequest { key_id }.
 * @param {string} key_id
 * @returns {Uint8Array}
 */
export function encodeApiKeyRevokeRequest(key_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(key_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeApiKeyRevokeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ApiKeyRotateRequest { key_uid }.
 * @param {string} key_uid
 * @returns {Uint8Array}
 */
export function encodeApiKeyRotateRequest(key_uid) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(key_uid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeApiKeyRotateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ApiKeyScopeClearRequest { key_uid, resource_type, resource_id }.
 * @param {string} key_uid
 * @param {string} resource_type
 * @param {string} resource_id
 * @returns {Uint8Array}
 */
export function encodeApiKeyScopeClearRequest(key_uid, resource_type, resource_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(key_uid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(resource_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(resource_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeApiKeyScopeClearRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ApiKeyScopeListRequest { key_uid }.
 * @param {string} key_uid
 * @returns {Uint8Array}
 */
export function encodeApiKeyScopeListRequest(key_uid) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(key_uid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeApiKeyScopeListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ApiKeyScopeSetRequest { key_uid, resource_type, resource_id, access_level }.
 * @param {string} key_uid
 * @param {string} resource_type
 * @param {string} resource_id
 * @param {string} access_level
 * @returns {Uint8Array}
 */
export function encodeApiKeyScopeSetRequest(key_uid, resource_type, resource_id, access_level) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(key_uid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(resource_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(resource_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(access_level, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeApiKeyScopeSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AddonUiBody(ReqApplicationsList) — lista aplikacji widocznych
 * w glownym menu launcher. Frontend buduje liste ikon w app menu.
 * MessageBody::AddonUiBody(ReqAppsList) — zunifikowana lista aplikacji
 * (native + WASM) filtrowana serwerowo; zasila launcher i sidebar.
 * @returns {Uint8Array}
 */
export function encodeAppsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAppsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AuditLogCleanupRequest — usun wpisy starsze niz N dni.
 * @param {number} keep_days
 * @returns {Uint8Array}
 */
export function encodeAuditLogCleanupRequest(keep_days) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAuditLogCleanupRequest(retptr, keep_days);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AuditLogExportRequest — eksport CSV z filtrami.
 * @param {string | null} [user_id]
 * @param {string | null} [addon_id]
 * @param {string | null} [action]
 * @param {string | null} [from_date]
 * @param {string | null} [to_date]
 * @param {string | null} [search]
 * @returns {Uint8Array}
 */
export function encodeAuditLogExportRequest(user_id, addon_id, action, from_date, to_date, search) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(user_id) ? 0 : passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(addon_id) ? 0 : passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(action) ? 0 : passStringToWasm0(action, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(from_date) ? 0 : passStringToWasm0(from_date, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(to_date) ? 0 : passStringToWasm0(to_date, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(search) ? 0 : passStringToWasm0(search, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        wasm.encodeAuditLogExportRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AuditLogListRequest — lista logu z filtrami + paginacja.
 * @param {string | null | undefined} user_id
 * @param {string | null | undefined} addon_id
 * @param {string | null | undefined} action
 * @param {string | null | undefined} from_date
 * @param {string | null | undefined} to_date
 * @param {string | null | undefined} search
 * @param {number} offset
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeAuditLogListRequest(user_id, addon_id, action, from_date, to_date, search, offset, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(user_id) ? 0 : passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(addon_id) ? 0 : passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(action) ? 0 : passStringToWasm0(action, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(from_date) ? 0 : passStringToWasm0(from_date, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(to_date) ? 0 : passStringToWasm0(to_date, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(search) ? 0 : passStringToWasm0(search, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        wasm.encodeAuditLogListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, offset, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AuthLoginRequest { username, password }.
 * @param {string} username
 * @param {string} password
 * @returns {Uint8Array}
 */
export function encodeAuthLoginRequest(username, password) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAuthLoginRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::AuthMeRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeAuthMeRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeAuthMeRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} current_password
 * @param {string} new_password
 * @returns {Uint8Array}
 */
export function encodeAuthPasswordChangeRequest(current_password, new_password) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(current_password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(new_password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeAuthPasswordChangeRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeBaselineAdoptClearRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeBaselineAdoptClearRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} donor_node_id
 * @returns {Uint8Array}
 */
export function encodeBaselineAdoptStartRequest(donor_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(donor_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBaselineAdoptStartRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeBaselineAdoptStatusRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeBaselineAdoptStatusRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeBaselineDonorListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeBaselineDonorListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkCancelRunRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkCancelRunRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkDeleteRequest(id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkGetRequest(id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeBenchmarkListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeBenchmarkListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} benchmark_id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkListRunsRequest(benchmark_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(benchmark_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkListRunsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeBenchmarkRecentRunsRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeBenchmarkRecentRunsRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkRunResultsRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkRunResultsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkRunStatusRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkRunStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkRunStreamRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkRunStreamRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `targets_json` to JSON-owa tablica obiektów TargetInputWire (id, kind,
 * service_ref, api_type, host, port, api_key?, model, label). `api_key` obecny
 * tylko gdy użytkownik wpisał nowy sekret — nie wraca w odczycie.
 * @param {string | null | undefined} id
 * @param {string} name
 * @param {string} config_json
 * @param {string} targets_json
 * @returns {Uint8Array}
 */
export function encodeBenchmarkSaveRequest(id, name, config_json, targets_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(id) ? 0 : passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(config_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(targets_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkSaveRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} benchmark_id
 * @returns {Uint8Array}
 */
export function encodeBenchmarkStartRunRequest(benchmark_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(benchmark_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBenchmarkStartRunRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::BrowserCaptureRequest — one-shot capture of the bot's page.
 * @param {number} session_id
 * @param {string} kind
 * @param {boolean} full_page
 * @returns {Uint8Array}
 */
export function encodeBrowserCaptureRequest(session_id, kind, full_page) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBrowserCaptureRequest(retptr, session_id, ptr0, len0, full_page);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} topic
 * @returns {Uint8Array}
 */
export function encodeBusAclListRequest(instance_id, topic) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusAclListRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `access_level` is 'allow' | 'deny' | 'clear' (see `bus.rs`'s doc).
 * @param {string} instance_id
 * @param {string} topic
 * @param {string} subject_type
 * @param {string} subject_id
 * @param {string} access_level
 * @returns {Uint8Array}
 */
export function encodeBusAclSetRequest(instance_id, topic, subject_type, subject_id, access_level) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(access_level, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeBusAclSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Follow-up toru P task 5: permission introspection for the UI (one round
 * trip on module mount).
 * @param {string} instance_id
 * @returns {Uint8Array}
 */
export function encodeBusCapabilitiesRequest(instance_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBusCapabilitiesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} source_topic
 * @param {number} partition
 * @param {bigint} offset
 * @returns {Uint8Array}
 */
export function encodeBusDlqDiscardRequest(instance_id, source_topic, partition, offset) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusDlqDiscardRequest(retptr, ptr0, len0, ptr1, len1, partition, offset);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} source_topic
 * @param {bigint | null | undefined} from_offset
 * @param {number} limit
 * @param {string | null} [from_offsets_json]
 * @param {number | null} [partition]
 * @returns {Uint8Array}
 */
export function encodeBusDlqListRequest(instance_id, source_topic, from_offset, limit, from_offsets_json, partition) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(from_offsets_json) ? 0 : passStringToWasm0(from_offsets_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeBusDlqListRequest(retptr, ptr0, len0, ptr1, len1, !isLikeNone(from_offset), isLikeNone(from_offset) ? BigInt(0) : from_offset, limit, ptr2, len2, isLikeNone(partition) ? Number.MAX_SAFE_INTEGER : (partition) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} source_topic
 * @param {number} max_records
 * @returns {Uint8Array}
 */
export function encodeBusDlqRetryAllRequest(instance_id, source_topic, max_records) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusDlqRetryAllRequest(retptr, ptr0, len0, ptr1, len1, max_records);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} source_topic
 * @param {number} partition
 * @param {bigint} offset
 * @returns {Uint8Array}
 */
export function encodeBusDlqRetryRequest(instance_id, source_topic, partition, offset) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusDlqRetryRequest(retptr, ptr0, len0, ptr1, len1, partition, offset);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `direction` is 'write' | 'read'.
 * @param {string} instance_id
 * @param {string} topic
 * @param {string} subject_type
 * @param {string} subject_id
 * @param {string} direction
 * @returns {Uint8Array}
 */
export function encodeBusFieldPolicyDeleteRequest(instance_id, topic, subject_type, subject_id, direction) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(direction, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeBusFieldPolicyDeleteRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Lists every field policy configured on `topic`.
 * @param {string} instance_id
 * @param {string} topic
 * @returns {Uint8Array}
 */
export function encodeBusFieldPolicyListRequest(instance_id, topic) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusFieldPolicyListRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `direction` is 'write' | 'read'; `required_fields` must be a subset of
 * `fields` (validated server-side).
 * @param {string} instance_id
 * @param {string} topic
 * @param {string} subject_type
 * @param {string} subject_id
 * @param {string} direction
 * @param {string[]} fields
 * @param {string[]} required_fields
 * @returns {Uint8Array}
 */
export function encodeBusFieldPolicySetRequest(instance_id, topic, subject_type, subject_id, direction, fields, required_fields) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(direction, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passArrayJsValueToWasm0(fields, wasm.__wbindgen_export);
        const len5 = WASM_VECTOR_LEN;
        const ptr6 = passArrayJsValueToWasm0(required_fields, wasm.__wbindgen_export);
        const len6 = WASM_VECTOR_LEN;
        wasm.encodeBusFieldPolicySetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v8 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v8;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} group
 * @param {string} topic
 * @returns {Uint8Array}
 */
export function encodeBusGroupDetailRequest(instance_id, group, topic) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(group, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusGroupDetailRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @returns {Uint8Array}
 */
export function encodeBusGroupListRequest(instance_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBusGroupListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} group
 * @param {string} topic
 * @returns {Uint8Array}
 */
export function encodeBusGroupPauseRequest(instance_id, group, topic) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(group, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusGroupPauseRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} group
 * @param {string} topic
 * @returns {Uint8Array}
 */
export function encodeBusGroupResumeRequest(instance_id, group, topic) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(group, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusGroupResumeRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Admin-triggered leader transfer for one partition (M03/M06 "Przenieś
 * lidera").
 * @param {string} instance_id
 * @param {string} topic
 * @param {number} partition
 * @param {string} target_node_id
 * @returns {Uint8Array}
 */
export function encodeBusLeaderTransferRequest(instance_id, topic, partition, target_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(target_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusLeaderTransferRequest(retptr, ptr0, len0, ptr1, len1, partition, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} topic
 * @param {bigint | null | undefined} from_offset
 * @param {number} limit
 * @param {string | null} [from_offsets_json]
 * @param {number | null} [partition]
 * @returns {Uint8Array}
 */
export function encodeBusMessagesBrowseRequest(instance_id, topic, from_offset, limit, from_offsets_json, partition) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(from_offsets_json) ? 0 : passStringToWasm0(from_offsets_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeBusMessagesBrowseRequest(retptr, ptr0, len0, ptr1, len1, !isLikeNone(from_offset), isLikeNone(from_offset) ? BigInt(0) : from_offset, limit, ptr2, len2, isLikeNone(partition) ? Number.MAX_SAFE_INTEGER : (partition) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `mode` is 'earliest' | 'latest' | 'explicit' | 'timestamp' (PLAN M04's
 * reset-offset modal); `offset` is required only for 'explicit', `ts_ms`
 * only for 'timestamp' (follow-up toru P task 4) — both ignored otherwise.
 * @param {string} instance_id
 * @param {string} group
 * @param {string} topic
 * @param {number} partition
 * @param {string} mode
 * @param {bigint | null} [offset]
 * @param {bigint | null} [ts_ms]
 * @returns {Uint8Array}
 */
export function encodeBusOffsetResetRequest(instance_id, group, topic, partition, mode, offset, ts_ms) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(group, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeBusOffsetResetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, partition, ptr3, len3, !isLikeNone(offset), isLikeNone(offset) ? BigInt(0) : offset, !isLikeNone(ts_ms), isLikeNone(ts_ms) ? BigInt(0) : ts_ms);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @returns {Uint8Array}
 */
export function encodeBusQuotaGetRequest(instance_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBusQuotaGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `max_groups` is `None` to leave the org's group ceiling unchanged
 * (follow-up toru P task 6/7 — see `QuotaSetRequest::max_groups`'s doc).
 * @param {string} instance_id
 * @param {number} max_topics
 * @param {number} max_partitions
 * @param {bigint} max_bytes_total
 * @param {number} produce_msgs_per_sec
 * @param {bigint} produce_bytes_per_sec
 * @param {number | null} [max_groups]
 * @returns {Uint8Array}
 */
export function encodeBusQuotaSetRequest(instance_id, max_topics, max_partitions, max_bytes_total, produce_msgs_per_sec, produce_bytes_per_sec, max_groups) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBusQuotaSetRequest(retptr, ptr0, len0, max_topics, max_partitions, max_bytes_total, produce_msgs_per_sec, produce_bytes_per_sec, isLikeNone(max_groups) ? Number.MAX_SAFE_INTEGER : (max_groups) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Admin-triggered replica-set change (M06 "Zmień repliki"). `partition:
 * None` targets every partition of `topic`; `Some(n)` targets one.
 * @param {string} instance_id
 * @param {string} topic
 * @param {number | null | undefined} partition
 * @param {string[]} replicas
 * @returns {Uint8Array}
 */
export function encodeBusReassignRequest(instance_id, topic, partition, replicas) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayJsValueToWasm0(replicas, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusReassignRequest(retptr, ptr0, len0, ptr1, len1, isLikeNone(partition) ? Number.MAX_SAFE_INTEGER : (partition) >>> 0, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `topic: None`/absent lists node cards + failovers for every topic in the
 * caller's org; `Some(name)` narrows the per-partition role matrix (and,
 * on a single node with no `ReplicationCoordinator` installed, the node
 * card's role counts too) to that one topic.
 * @param {string} instance_id
 * @param {string | null} [topic]
 * @returns {Uint8Array}
 */
export function encodeBusReplicaListRequest(instance_id, topic) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(topic) ? 0 : passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeBusReplicaListRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `compatibility` is 'none' | 'backward' | 'forward' | 'full'.
 * @param {string} instance_id
 * @param {string} subject
 * @param {string} compatibility
 * @returns {Uint8Array}
 */
export function encodeBusSchemaCompatibilitySetRequest(instance_id, subject, compatibility) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(compatibility, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusSchemaCompatibilitySetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `version: None` deletes every version of `subject`; `deprecate_only`
 * marks it deprecated instead of a hard delete.
 * @param {string} instance_id
 * @param {string} subject
 * @param {number | null | undefined} version
 * @param {boolean} deprecate_only
 * @returns {Uint8Array}
 */
export function encodeBusSchemaDeleteRequest(instance_id, subject, version, deprecate_only) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusSchemaDeleteRequest(retptr, ptr0, len0, ptr1, len1, isLikeNone(version) ? Number.MAX_SAFE_INTEGER : (version) >>> 0, deprecate_only);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Derives a read-projected sub-schema from a stored field policy
 * (PLAN-F3 §5.4) — `version: None` resolves to `subject`'s latest
 * version; `direction` is 'write' | 'read'.
 * @param {string} instance_id
 * @param {string} subject
 * @param {number | null | undefined} version
 * @param {string} topic
 * @param {string} subject_type
 * @param {string} subject_id
 * @param {string} direction
 * @returns {Uint8Array}
 */
export function encodeBusSchemaDerivedGetRequest(instance_id, subject, version, topic, subject_type, subject_id, direction) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(topic, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passStringToWasm0(direction, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len5 = WASM_VECTOR_LEN;
        wasm.encodeBusSchemaDerivedGetRequest(retptr, ptr0, len0, ptr1, len1, isLikeNone(version) ? Number.MAX_SAFE_INTEGER : (version) >>> 0, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `version: None` resolves to `subject`'s latest version.
 * @param {string} instance_id
 * @param {string} subject
 * @param {number | null} [version]
 * @returns {Uint8Array}
 */
export function encodeBusSchemaGetRequest(instance_id, subject, version) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusSchemaGetRequest(retptr, ptr0, len0, ptr1, len1, isLikeNone(version) ? Number.MAX_SAFE_INTEGER : (version) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `schema_type` is 'json_schema' | 'avro' | 'protobuf' | 'thrift';
 * `compatibility: None` leaves/defaults to the subject's existing (or
 * 'none' on first registration) compatibility mode.
 * @param {string} instance_id
 * @param {string} subject
 * @param {string} schema_type
 * @param {string} schema_text
 * @param {string | null} [compatibility]
 * @returns {Uint8Array}
 */
export function encodeBusSchemaRegisterRequest(instance_id, subject, schema_type, schema_text, compatibility) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(schema_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(schema_text, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(compatibility) ? 0 : passStringToWasm0(compatibility, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeBusSchemaRegisterRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Lists every schema subject registered in the caller's org.
 * @param {string} instance_id
 * @returns {Uint8Array}
 */
export function encodeBusSchemaSubjectListRequest(instance_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBusSchemaSubjectListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Lists every immutable version registered under `subject`.
 * @param {string} instance_id
 * @param {string} subject
 * @returns {Uint8Array}
 */
export function encodeBusSchemaVersionListRequest(instance_id, subject) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusSchemaVersionListRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @returns {Uint8Array}
 */
export function encodeBusStatsSnapshotRequest(instance_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBusStatsSnapshotRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `options_json` is a JSON object matching `BusTopicOptionsWire` field
 * names (snake_case) — every field optional, absent = server default (see
 * `bus.rs`'s module doc). Same JSON-passthrough pattern as
 * `encodeBenchmarkSaveRequest`'s `targets_json`.
 * @param {string} instance_id
 * @param {string} name
 * @param {string} options_json
 * @returns {Uint8Array}
 */
export function encodeBusTopicCreateRequest(instance_id, name, options_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(options_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusTopicCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} name
 * @returns {Uint8Array}
 */
export function encodeBusTopicDeleteRequest(instance_id, name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusTopicDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} name
 * @returns {Uint8Array}
 */
export function encodeBusTopicDetailRequest(instance_id, name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeBusTopicDetailRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @returns {Uint8Array}
 */
export function encodeBusTopicListRequest(instance_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeBusTopicListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} instance_id
 * @param {string} name
 * @param {string} options_json
 * @returns {Uint8Array}
 */
export function encodeBusTopicUpdateRequest(instance_id, name, options_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(options_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeBusTopicUpdateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CameraAdminBody(AddOnvifRequest) — bind a discovered ONVIF
 * device as a managed camera session. Credentials travel over the TLS
 * admin transport and are AES-GCM-sealed server-side before persistence.
 * @param {string} display_name
 * @param {string} device_service_url
 * @param {string} username
 * @param {string} password
 * @param {string | null} [profile_token]
 * @param {number | null} [target_fps]
 * @returns {Uint8Array}
 */
export function encodeCameraAddOnvifRequest(display_name, device_service_url, username, password, profile_token, target_fps) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(display_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(device_service_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(profile_token) ? 0 : passStringToWasm0(profile_token, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeCameraAddOnvifRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, isLikeNone(target_fps) ? Number.MAX_SAFE_INTEGER : (target_fps) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CameraAdminBody(DetectionsSubscribeRequest) — open a per-camera
 * detection overlay stream. The handler validates the `cam_<uuid v4>` id,
 * gates on `camera.read` + org isolation, and replies with a long-lived
 * stream of `CameraDetectionsFrame` chunks until cancel/disconnect.
 * @param {string} camera_id
 * @returns {Uint8Array}
 */
export function encodeCameraDetectionsSubscribeRequest(camera_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(camera_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCameraDetectionsSubscribeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CameraAdminBody(DiscoverRequest) — kick off ONVIF WS-Discovery
 * against the local network; the response carries the discovered devices.
 * @returns {Uint8Array}
 */
export function encodeCameraDiscoverRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeCameraDiscoverRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CameraAdminBody(FrameUrlRequest) — live-preview tile URL
 * for `<tf-live-camera-tile>`. The handler gates on `camera.read`,
 * enforces UUID v4 camera_id validation, a per-user rate limit, and a
 * 5..=300 s dispatch TTL band before minting against the global frame
 * signed-URL issuer.
 * @param {string} camera_id
 * @param {number} ttl_secs
 * @returns {Uint8Array}
 */
export function encodeCameraFrameUrlRequest(camera_id, ttl_secs) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(camera_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCameraFrameUrlRequest(retptr, ptr0, len0, ttl_secs);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string | null | undefined} surface_filter
 * @param {boolean} include_blocking_diagnostics
 * @returns {Uint8Array}
 */
export function encodeCatalogListRequest(surface_filter, include_blocking_diagnostics) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(surface_filter) ? 0 : passStringToWasm0(surface_filter, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeCatalogListRequest(retptr, ptr0, len0, include_blocking_diagnostics);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ChatStreamRequest — przyjmuje JSON string messages, parsuje
 * jako JsValue. Bootstrap accepts tylko `model_id` + jednoelementowa lista
 * user messages. Pelny messages[] input po integracji serde-wasm-bindgen (#36 ph.2).
 * @param {string} model_id
 * @param {string} user_message
 * @param {string | null} [flow_id]
 * @param {string | null} [session_id]
 * @returns {Uint8Array}
 */
export function encodeChatStreamRequestSimple(model_id, user_message, flow_id, session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(user_message, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(flow_id) ? 0 : passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(session_id) ? 0 : passStringToWasm0(session_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeChatStreamRequestSimple(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterAddMemberRequest.
 * @param {string} cluster_id
 * @param {string} node_id
 * @param {string | null} [interface_type]
 * @param {number | null} [interface_speed_mbps]
 * @param {string | null} [interface_name]
 * @param {string | null} [interface_ip]
 * @returns {Uint8Array}
 */
export function encodeClusterAddMemberRequest(cluster_id, node_id, interface_type, interface_speed_mbps, interface_name, interface_ip) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(interface_type) ? 0 : passStringToWasm0(interface_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(interface_name) ? 0 : passStringToWasm0(interface_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(interface_ip) ? 0 : passStringToWasm0(interface_ip, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeClusterAddMemberRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, isLikeNone(interface_speed_mbps) ? Number.MAX_SAFE_INTEGER : (interface_speed_mbps) >>> 0, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterCreateRequest.
 * @param {string} name
 * @param {string | null | undefined} description
 * @param {string} strategy
 * @param {boolean} failover_enabled
 * @param {string | null | undefined} failover_target
 * @param {number} health_check_interval_ms
 * @param {number} timeout_ms
 * @returns {Uint8Array}
 */
export function encodeClusterCreateRequest(name, description, strategy, failover_enabled, failover_target, health_check_interval_ms, timeout_ms) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(strategy, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(failover_target) ? 0 : passStringToWasm0(failover_target, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeClusterCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, failover_enabled, ptr3, len3, health_check_interval_ms, timeout_ms);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterDeleteRequest { cluster_id }.
 * @param {string} cluster_id
 * @returns {Uint8Array}
 */
export function encodeClusterDeleteRequest(cluster_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeClusterDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterDeployRequest — deploy one model split across the whole
 * cluster (vLLM tensor-parallel). Optional fields fall back to backend defaults
 * when passed `None`.
 * @param {string} cluster_id
 * @param {string} engine_id
 * @param {string | null} [model_repo]
 * @param {string | null} [model_preset_id]
 * @param {string | null} [served_model_name]
 * @param {number | null} [gpu_memory_utilization]
 * @param {number | null} [max_model_len]
 * @param {number | null} [port]
 * @param {number | null} [gpus_per_node]
 * @param {string | null} [config_json]
 * @param {number | null} [gcs_timeout_secs]
 * @param {number | null} [ready_timeout_secs]
 * @param {number | null} [build_timeout_secs]
 * @param {number | null} [prompt_per_1k]
 * @param {number | null} [completion_per_1k]
 * @param {number | null} [audio_per_min]
 * @param {number | null} [image_each]
 * @returns {Uint8Array}
 */
export function encodeClusterDeployRequest(cluster_id, engine_id, model_repo, model_preset_id, served_model_name, gpu_memory_utilization, max_model_len, port, gpus_per_node, config_json, gcs_timeout_secs, ready_timeout_secs, build_timeout_secs, prompt_per_1k, completion_per_1k, audio_per_min, image_each) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(engine_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(model_repo) ? 0 : passStringToWasm0(model_repo, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(model_preset_id) ? 0 : passStringToWasm0(model_preset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(served_model_name) ? 0 : passStringToWasm0(served_model_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(config_json) ? 0 : passStringToWasm0(config_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        wasm.encodeClusterDeployRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, isLikeNone(gpu_memory_utilization) ? Number.MAX_SAFE_INTEGER : Math.fround(gpu_memory_utilization), isLikeNone(max_model_len) ? Number.MAX_SAFE_INTEGER : (max_model_len) >>> 0, isLikeNone(port) ? 0xFFFFFF : port, isLikeNone(gpus_per_node) ? Number.MAX_SAFE_INTEGER : (gpus_per_node) >>> 0, ptr5, len5, isLikeNone(gcs_timeout_secs) ? Number.MAX_SAFE_INTEGER : (gcs_timeout_secs) >>> 0, isLikeNone(ready_timeout_secs) ? Number.MAX_SAFE_INTEGER : (ready_timeout_secs) >>> 0, isLikeNone(build_timeout_secs) ? Number.MAX_SAFE_INTEGER : (build_timeout_secs) >>> 0, !isLikeNone(prompt_per_1k), isLikeNone(prompt_per_1k) ? 0 : prompt_per_1k, !isLikeNone(completion_per_1k), isLikeNone(completion_per_1k) ? 0 : completion_per_1k, !isLikeNone(audio_per_min), isLikeNone(audio_per_min) ? 0 : audio_per_min, !isLikeNone(image_each), isLikeNone(image_each) ? 0 : image_each);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterDeployStopRequest { cluster_id, deployment_cluster_id }.
 * @param {string} cluster_id
 * @param {string} deployment_cluster_id
 * @returns {Uint8Array}
 */
export function encodeClusterDeployStopRequest(cluster_id, deployment_cluster_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(deployment_cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeClusterDeployStopRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterDetailRequest { cluster_id }.
 * @param {string} cluster_id
 * @returns {Uint8Array}
 */
export function encodeClusterDetailRequest(cluster_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeClusterDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeClusterListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeClusterListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterProbeStreamRequest { node_ids }.
 * @param {string[]} node_ids
 * @returns {Uint8Array}
 */
export function encodeClusterProbeStreamRequest(node_ids) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArrayJsValueToWasm0(node_ids, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeClusterProbeStreamRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterRdmaConfigureRequest { cluster_id, sudo_password, mtu }.
 * @param {string} cluster_id
 * @param {string} sudo_password
 * @param {number | null} [mtu]
 * @returns {Uint8Array}
 */
export function encodeClusterRdmaConfigureRequest(cluster_id, sudo_password, mtu) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sudo_password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeClusterRdmaConfigureRequest(retptr, ptr0, len0, ptr1, len1, isLikeNone(mtu) ? Number.MAX_SAFE_INTEGER : (mtu) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterRemoveMemberRequest.
 * @param {string} cluster_id
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeClusterRemoveMemberRequest(cluster_id, node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeClusterRemoveMemberRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ClusterUpdateRequest. Wszystkie pola opcjonalne — `None`
 * zachowuje obecna wartosc na serwerze.
 * @param {string} cluster_id
 * @param {string | null} [name]
 * @param {string | null} [description]
 * @param {string | null} [strategy]
 * @param {boolean | null} [failover_enabled]
 * @param {string | null} [failover_target]
 * @param {number | null} [health_check_interval_ms]
 * @param {number | null} [timeout_ms]
 * @returns {Uint8Array}
 */
export function encodeClusterUpdateRequest(cluster_id, name, description, strategy, failover_enabled, failover_target, health_check_interval_ms, timeout_ms) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(name) ? 0 : passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(strategy) ? 0 : passStringToWasm0(strategy, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(failover_target) ? 0 : passStringToWasm0(failover_target, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeClusterUpdateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, isLikeNone(failover_enabled) ? 0xFFFFFF : failover_enabled ? 1 : 0, ptr4, len4, isLikeNone(health_check_interval_ms) ? Number.MAX_SAFE_INTEGER : (health_check_interval_ms) >>> 0, isLikeNone(timeout_ms) ? Number.MAX_SAFE_INTEGER : (timeout_ms) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(ApprovalDecideRequest) — `always` on a
 * mandatory-interactive capability is refused server-side, not hidden in the UI.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioApprovalDecideRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioApprovalDecideRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(ApprovalsListRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioApprovalsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioApprovalsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(CodeSearchRequest) — a semantic request may be
 * answered by grep with `degraded=true`; grep stays authoritative.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioCodeSearchRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioCodeSearchRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(ExecCancelRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioExecCancelRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioExecCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(ExecOutputRequest) — the transcript of a
 * command, paged by `after_seq` over its lines.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioExecOutputRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioExecOutputRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(ExecStartRequest) — `argv` is always a vector; a
 * shell is one explicit, quoted argument.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioExecStartRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioExecStartRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileCreateRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileDeleteRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileGrepRequest) — `regex=false` keeps the query
 * literal, so a search for `a.b` is not silently a pattern.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileGrepRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileGrepRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileMkdirRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileMkdirRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileMkdirRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileReadRequest) — whole file or a line range.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileReadRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileReadRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileRenameRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileRenameRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileRenameRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileTreeRequest) — worktree listing; `path` is
 * relative, `depth` bounds the expansion.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileTreeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileTreeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(FileWriteRequest) — `expected_blob_sha` is the
 * compare-and-swap token; omitting it means the file must not exist yet.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioFileWriteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioFileWriteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitBranchesRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitBranchesRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitBranchesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitCommitRequest) — without `patch_set_id` the
 * PEP opens a review instead of committing.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitCommitRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitCommitRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitDiffRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitDiffRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitDiffRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitLogRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitLogRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitLogRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitMergeAbandonRequest) — drops a held
 * integration worktree and its private ref.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitMergeAbandonRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitMergeAbandonRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitMergeFinalizeRequest) — commits the accepted
 * blobs and moves the target ref under compare-and-swap.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitMergeFinalizeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitMergeFinalizeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitMergeRequest) — starts the integration
 * worktree; a conflict comes back as a result, not an error.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitMergeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitMergeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitPushRequest) — mandatory-interactive, so the
 * answer may be an approval question rather than a push.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitPushRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitPushRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitStatusRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitStatusRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(GitSyncRequest) — `mode` is `fetch` or `pull`.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioGitSyncRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioGitSyncRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(IndexRebuildRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioIndexRebuildRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioIndexRebuildRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(IndexStatusRequest) — per-branch index state.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioIndexStatusRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioIndexStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(IndexStreamRequest) — STREAM-INITIATING: end =
 * IndexStreamEnd. The semantic index is phase 7, so today the stream closes
 * with `index_unavailable` instead of reporting progress.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioIndexStreamRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioIndexStreamRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(OperationResolveRequest) — settles an `unknown`
 * operation as completed or failed. There is no silent retry.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioOperationResolveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioOperationResolveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(PatchBlobGetRequest) — one CAS blob by digest,
 * so a partially accepted file can be rebuilt whole instead of from hunk
 * windows.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioPatchBlobGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioPatchBlobGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(PatchDecideRequest) — decisions per file and per
 * hunk, including `request_revision` with a note.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioPatchDecideRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioPatchDecideRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(PatchSetAbandonRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioPatchSetAbandonRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioPatchSetAbandonRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(PatchSetGetRequest) — files with their hunks.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioPatchSetGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioPatchSetGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(PatchSetsListRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioPatchSetsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioPatchSetsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(ProjectLinkListRequest) — projects linked to
 * this workspace (§20).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioProjectLinkListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioProjectLinkListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(ProjectLinkSetRequest) — `linked` picks the
 * direction; the answer is the whole ProjectLinkListResponse, not a delta.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioProjectLinkSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioProjectLinkSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(RepoTreeRequest) — structure of a PINNED commit
 * of a linked workspace. Addressed by a commit id, never by a path on the
 * owner node.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioRepoTreeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioRepoTreeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionAutonomySetRequest) — raising is clamped
 * to the workspace ceiling by the server.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionAutonomySetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionAutonomySetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionCancelRequest) — without `run_id` the
 * whole session, subagent runs included.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionCancelRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionCloseRequest) — usuwa worktree, zostawia
 * galaz.
 * @param {string} workspace_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionCloseRequest(workspace_id, session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionCloseRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionGrantRevokeRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionGrantRevokeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionGrantRevokeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionGrantsListRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionGrantsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionGrantsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionMessageSendRequest) — a user turn to the
 * session's root agent.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionMessageSendRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionMessageSendRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionOpenRequest) — galaz jest wyprowadzana
 * serwerowo, nie podawana z UI.
 * @param {string} workspace_id
 * @param {string} title
 * @param {string} autonomy_mode
 * @param {bigint | null} [agent_service_id]
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionOpenRequest(workspace_id, title, autonomy_mode, agent_service_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(title, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(autonomy_mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionOpenRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, !isLikeNone(agent_service_id), isLikeNone(agent_service_id) ? BigInt(0) : agent_service_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionOperationsRequest) — the effect journal;
 * `status="unknown"` lists what needs a human decision.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionOperationsRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionOperationsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionRunsRequest) — the revision chain with the
 * trigger of every turn.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionRunsRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionRunsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionStreamRequest) — STREAM-INITIATING (no
 * plain response): chunks = SessionStreamEvent, end = SessionStreamEnd.
 * `after_seq` is the resume cursor; 0 replays the session from its first event.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionStreamRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionStreamRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionTasksRequest) — the session's plan: the
 * task rows the build loop's gate checks before it lets the work finish.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionTasksRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionTasksRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionTimelineRequest) — `after_seq` is the
 * cursor; events are the source of truth, the status columns a projection.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionTimelineRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionTimelineRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(SessionsListRequest) — sesje WOLAJACEGO w tym
 * workspace; serwer filtruje po uzytkowniku, bez obejscia dla administratora.
 * @param {string} workspace_id
 * @returns {Uint8Array}
 */
export function encodeCodeStudioSessionsListRequest(workspace_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioSessionsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(TerminalCloseRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioTerminalCloseRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioTerminalCloseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(TerminalInputRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioTerminalInputRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioTerminalInputRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(TerminalOpenRequest) — the VT machine lives on
 * the owner node; the browser only sends keys and renders cells.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioTerminalOpenRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioTerminalOpenRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(TerminalResizeRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioTerminalResizeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioTerminalResizeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(TerminalSnapshotRequest) — full grid after a
 * reload or reconnect; the live stream carries only changed rows.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioTerminalSnapshotRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioTerminalSnapshotRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(TerminalStreamRequest) — STREAM-INITIATING:
 * chunks = TerminalStreamSnapshot / TerminalStreamDelta, end =
 * TerminalStreamEnd. `after_revision` is the VT revision the client holds; any
 * value other than the live one earns a full snapshot first.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioTerminalStreamRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioTerminalStreamRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(TerminalsListRequest) — shells already open in
 * the session, so a browser reload rebuilds the dock instead of opening a
 * second shell next to a running one.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioTerminalsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioTerminalsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceAllowlistListRequest) — the `always`
 * scope, workspace-wide.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceAllowlistListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceAllowlistListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceAllowlistRemoveRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceAllowlistRemoveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceAllowlistRemoveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceAllowlistSetRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceAllowlistSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceAllowlistSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceArchiveRequest).
 * @param {string} workspace_id
 * @param {boolean} archived
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceArchiveRequest(workspace_id, archived) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceArchiveRequest(retptr, ptr0, len0, archived);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceCreateRequest). `members_json` to
 * tablica JSON obiektow `{user_id, role}` (jak `members_json` w Project
 * Studio); `secret_material` jedzie RAZ i nigdy nie wraca.
 * @param {string} name
 * @param {string} node_id
 * @param {string} exec_mode
 * @param {string | null | undefined} container_image
 * @param {string} repo_kind
 * @param {string | null | undefined} repo_url
 * @param {string | null | undefined} repo_auth_kind
 * @param {string | null | undefined} secret_material
 * @param {string | null | undefined} ssh_host_fingerprint
 * @param {string | null | undefined} default_branch
 * @param {string} autonomy_ceiling
 * @param {string} egress_policy
 * @param {boolean} index_enabled
 * @param {string} members_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceCreateRequest(name, node_id, exec_mode, container_image, repo_kind, repo_url, repo_auth_kind, secret_material, ssh_host_fingerprint, default_branch, autonomy_ceiling, egress_policy, index_enabled, members_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(exec_mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(container_image) ? 0 : passStringToWasm0(container_image, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(repo_kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(repo_url) ? 0 : passStringToWasm0(repo_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        var ptr6 = isLikeNone(repo_auth_kind) ? 0 : passStringToWasm0(repo_auth_kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len6 = WASM_VECTOR_LEN;
        var ptr7 = isLikeNone(secret_material) ? 0 : passStringToWasm0(secret_material, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len7 = WASM_VECTOR_LEN;
        var ptr8 = isLikeNone(ssh_host_fingerprint) ? 0 : passStringToWasm0(ssh_host_fingerprint, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len8 = WASM_VECTOR_LEN;
        var ptr9 = isLikeNone(default_branch) ? 0 : passStringToWasm0(default_branch, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len9 = WASM_VECTOR_LEN;
        const ptr10 = passStringToWasm0(autonomy_ceiling, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len10 = WASM_VECTOR_LEN;
        const ptr11 = passStringToWasm0(egress_policy, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len11 = WASM_VECTOR_LEN;
        const ptr12 = passStringToWasm0(members_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len12 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6, ptr7, len7, ptr8, len8, ptr9, len9, ptr10, len10, ptr11, len11, index_enabled, ptr12, len12);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v14 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v14;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceCreatorGrantSetRequest) — prawo
 * zakladania workspace'ow, nadawane per uzytkownik.
 * @param {string} user_id
 * @param {boolean} granted
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceCreatorGrantSetRequest(user_id, granted) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceCreatorGrantSetRequest(retptr, ptr0, len0, granted);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceDeleteRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceGetRequest) — szczegoly + czlonkowie +
 * kroki provisioningu.
 * @param {string} workspace_id
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceGetRequest(workspace_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceMemberCandidatesRequest) — users the
 * caller may still add. `workspace_id: null` is the creation wizard, which has
 * no workspace whose members could be excluded yet.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceMemberCandidatesRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceMemberCandidatesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceMemberRemoveRequest).
 * @param {string} workspace_id
 * @param {string} user_id
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceMemberRemoveRequest(workspace_id, user_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceMemberRemoveRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceMemberSetRequest) — dodanie albo zmiana
 * roli czlonka.
 * @param {string} workspace_id
 * @param {string} user_id
 * @param {string} role
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceMemberSetRequest(workspace_id, user_id, role) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(role, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceMemberSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceRetryRequest) — wznowienie
 * provisioningu; kroki `done` sa pomijane, wiec to wznowienie, nie przebudowa.
 * @param {string} workspace_id
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceRetryRequest(workspace_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(workspace_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceRetryRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceSecretSetRequest) — credential material
 * travels once and never comes back.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceSecretSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceSecretSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspaceSettingsUpdateRequest) — `exec_mode` is
 * absent because the execution mode of an existing workspace is immutable.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspaceSettingsUpdateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorkspaceSettingsUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorkspacesListRequest) — lista workspace'ow.
 * @param {boolean} include_archived
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorkspacesListRequest(include_archived) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeCodeStudioWorkspacesListRequest(retptr, include_archived);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::CodeStudioBody(WorktreesListRequest).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeCodeStudioWorktreesListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeCodeStudioWorktreesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string | null} [status]
 * @param {string | null} [user_id]
 * @param {string | null} [addon_id]
 * @param {number | null} [limit]
 * @param {number | null} [offset]
 * @returns {Uint8Array}
 */
export function encodeComplianceAiEventsListRequest(status, user_id, addon_id, limit, offset) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(status) ? 0 : passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(user_id) ? 0 : passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(addon_id) ? 0 : passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeComplianceAiEventsListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, isLikeNone(limit) ? Number.MAX_SAFE_INTEGER : (limit) >>> 0, isLikeNone(offset) ? Number.MAX_SAFE_INTEGER : (offset) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeComplianceDataCategoriesListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeComplianceDataCategoriesListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeComplianceRetentionPoliciesListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeComplianceRetentionPoliciesListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::DashboardMetricsRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeDashboardMetricsRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeDashboardMetricsRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::DeployVllmRecommendRequest. Plynnie przyjmuje JSON
 * (pelne struct DeployVllmRecommendRequest serializowane przez GUI).
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeDeployVllmRecommendRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeDeployVllmRecommendRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} engine_id
 * @param {string} status
 * @param {boolean} only_mine
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeDeploymentListRequest(engine_id, status, only_mine, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(engine_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeDeploymentListRequest(retptr, ptr0, len0, ptr1, len1, only_mine, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} deploy_id
 * @param {boolean} replay_tail
 * @returns {Uint8Array}
 */
export function encodeDeploymentLogStreamRequest(deploy_id, replay_tail) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(deploy_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeDeploymentLogStreamRequest(retptr, ptr0, len0, replay_tail);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} deploy_id
 * @returns {Uint8Array}
 */
export function encodeDeploymentStatusRequest(deploy_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(deploy_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeDeploymentStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Buduje Envelope (routing=Direct) z podanymi polami + body bytes; zwraca
 * CBOR-zakodowany frame jako Uint8Array.
 *
 * `correlation_id` przekazywany jako u64 (BigInt po stronie JS).
 * @param {bigint} correlation_id
 * @param {bigint} sequence
 * @param {number} message_kind
 * @param {Uint8Array} body
 * @returns {Uint8Array}
 */
export function encodeEnvelopeDirect(correlation_id, sequence, message_kind, body) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(body, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeEnvelopeDirect(retptr, correlation_id, sequence, message_kind, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeEnvironmentExportBundleRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeEnvironmentExportBundleRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeEnvironmentGetKindRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeEnvironmentGetKindRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `confirm_environment_name` is REQUIRED (must equal the target
 * environment's name, uppercased) when the pull promotes UPWARD (in
 * particular onto Prod) — validated server-side (D-Z12.8).
 * `selected_resource_keys` are `"table:resource_id"` strings, one per
 * checked row in the diff table.
 * @param {string} pull_id
 * @param {string | null | undefined} confirm_environment_name
 * @param {string[]} selected_resource_keys
 * @returns {Uint8Array}
 */
export function encodeEnvironmentImportApplyRequest(pull_id, confirm_environment_name, selected_resource_keys) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(pull_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(confirm_environment_name) ? 0 : passStringToWasm0(confirm_environment_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayJsValueToWasm0(selected_resource_keys, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeEnvironmentImportApplyRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `archive_bytes` arrives from JS as a Uint8Array (the file the admin
 * picked via `tf-file-input`) and wasm-bindgen materializes it directly
 * into `Vec<u8>`.
 * @param {Uint8Array} archive_bytes
 * @returns {Uint8Array}
 */
export function encodeEnvironmentImportFromFileRequest(archive_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(archive_bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeEnvironmentImportFromFileRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} pull_id
 * @returns {Uint8Array}
 */
export function encodeEnvironmentImportPreviewDiffRequest(pull_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(pull_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeEnvironmentImportPreviewDiffRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeEnvironmentPullDonorListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeEnvironmentPullDonorListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} donor_node_id
 * @returns {Uint8Array}
 */
export function encodeEnvironmentPullStartRequest(donor_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(donor_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeEnvironmentPullStartRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} pull_id
 * @returns {Uint8Array}
 */
export function encodeEnvironmentPullStatusRequest(pull_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(pull_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeEnvironmentPullStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `new_kind`: "dev" | "test" | "prod". `confirm_environment_name` is
 * REQUIRED (must be exactly `"PROD"`) when switching to Prod — validated
 * server-side, this is not a client-side convenience.
 * @param {string} new_kind
 * @param {string | null} [confirm_environment_name]
 * @returns {Uint8Array}
 */
export function encodeEnvironmentSetKindRequest(new_kind, confirm_environment_name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(new_kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(confirm_environment_name) ? 0 : passStringToWasm0(confirm_environment_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeEnvironmentSetKindRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {boolean} strict
 * @returns {Uint8Array}
 */
export function encodeEnvironmentSetStrictIsolationRequest(strict) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeEnvironmentSetStrictIsolationRequest(retptr, strict);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::EventsBody(BrowseRequest) — filter as a JSON object.
 * @param {string} filter_json
 * @returns {Uint8Array}
 */
export function encodeEventsBrowseRequest(filter_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(filter_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeEventsBrowseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::EventsBody(RunRequest { run_id, after_seq, limit }) — the
 * timeline of ONE run, so the three scalars travel positionally.
 * @param {string} run_id
 * @param {number} after_seq
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeEventsRunRequest(run_id, after_seq, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeEventsRunRequest(retptr, ptr0, len0, after_seq, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FastPathListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeFastPathListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeFastPathListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowCreateRequest { name, description, graph_json,
 * published_model_name? }. `published_model_name = None` keeps the flow
 * private; passing a value publishes it on `/v1/models` after the
 * catalog rebuild — collisions with aliases / existing flows are
 * rejected by the handler before the row is written.
 * @param {string} name
 * @param {string | null | undefined} description
 * @param {string} graph_json
 * @param {string | null} [published_model_name]
 * @returns {Uint8Array}
 */
export function encodeFlowCreateRequest(name, description, graph_json, published_model_name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(graph_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(published_model_name) ? 0 : passStringToWasm0(published_model_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeFlowCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowDeleteRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowDeleteRequest(flow_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeFlowDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowDetailRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowDetailRequest(flow_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeFlowDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowExecutionsListRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowExecutionsListRequest(flow_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeFlowExecutionsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowFactoryRestoreRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowFactoryRestoreRequest(flow_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeFlowFactoryRestoreRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowInvokeRequest — uniwersalny most do flow engine. Wariant
 * audio-only dla chat audio (jedno wejście Audio). Multi-input dojdzie później.
 * @param {string | null | undefined} flow_id
 * @param {string} model
 * @param {string} service_type
 * @param {string} mime
 * @param {number | null | undefined} sample_rate
 * @param {Uint8Array} audio
 * @param {string | null | undefined} language
 * @param {string | null | undefined} session_id
 * @param {boolean} output_audio
 * @param {string | null} [stt_model]
 * @param {string | null} [tts_model]
 * @returns {Uint8Array}
 */
export function encodeFlowInvokeAudio(flow_id, model, service_type, mime, sample_rate, audio, language, session_id, output_audio, stt_model, tts_model) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(flow_id) ? 0 : passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(service_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(mime, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passArray8ToWasm0(audio, wasm.__wbindgen_export);
        const len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(language) ? 0 : passStringToWasm0(language, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        var ptr6 = isLikeNone(session_id) ? 0 : passStringToWasm0(session_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len6 = WASM_VECTOR_LEN;
        var ptr7 = isLikeNone(stt_model) ? 0 : passStringToWasm0(stt_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len7 = WASM_VECTOR_LEN;
        var ptr8 = isLikeNone(tts_model) ? 0 : passStringToWasm0(tts_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len8 = WASM_VECTOR_LEN;
        wasm.encodeFlowInvokeAudio(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, isLikeNone(sample_rate) ? Number.MAX_SAFE_INTEGER : (sample_rate) >>> 0, ptr4, len4, ptr5, len5, ptr6, len6, output_audio, ptr7, len7, ptr8, len8);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v10 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v10;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeFlowListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeFlowListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowNodeTemplatesListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeFlowNodeTemplatesListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeFlowNodeTemplatesListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowUpdateRequest — partial update flow. Pass
 * `publish_set=true, published_model_name=Some("foo")` to publish or
 * `publish_set=true, published_model_name=None` to un-publish; leave
 * `publish_set=false` to keep whatever the server has.
 * @param {string} flow_id
 * @param {string | null | undefined} name
 * @param {string | null | undefined} description
 * @param {string | null | undefined} flow_json
 * @param {string | null | undefined} status
 * @param {boolean} publish_set
 * @param {string | null} [published_model_name]
 * @returns {Uint8Array}
 */
export function encodeFlowUpdateRequest(flow_id, name, description, flow_json, status, publish_set, published_model_name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(name) ? 0 : passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(flow_json) ? 0 : passStringToWasm0(flow_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(status) ? 0 : passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(published_model_name) ? 0 : passStringToWasm0(published_model_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        wasm.encodeFlowUpdateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, publish_set, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowVersionGetRequest { flow_id, version_id }.
 * @param {string} flow_id
 * @param {string} version_id
 * @returns {Uint8Array}
 */
export function encodeFlowVersionGetRequest(flow_id, version_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(version_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeFlowVersionGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowVersionListRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowVersionListRequest(flow_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeFlowVersionListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::FlowVersionRestoreRequest { flow_id, version_id }.
 * @param {string} flow_id
 * @param {string} version_id
 * @returns {Uint8Array}
 */
export function encodeFlowVersionRestoreRequest(flow_id, version_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(version_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeFlowVersionRestoreRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::HubEngineListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeHubEngineListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeHubEngineListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::HubModelSearchRequest { query }.
 * @param {string} query
 * @returns {Uint8Array}
 */
export function encodeHubModelSearchRequest(query) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeHubModelSearchRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} resource_type
 * @param {string} resource_id
 * @param {string} subject_type
 * @param {string} subject_id
 * @returns {Uint8Array}
 */
export function encodeIamClearPermissionRequest(resource_type, resource_id, subject_type, subject_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(resource_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(resource_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeIamClearPermissionRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} name
 * @param {string} description
 * @returns {Uint8Array}
 */
export function encodeIamCreateGroupRequest(name, description) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeIamCreateGroupRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} username
 * @param {string} password
 * @param {string} display_name
 * @param {string} email
 * @param {string} role
 * @param {string} group_ids_csv
 * @returns {Uint8Array}
 */
export function encodeIamCreateUserRequest(username, password, display_name, email, role, group_ids_csv) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(username, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(display_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(email, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(role, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passStringToWasm0(group_ids_csv, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len5 = WASM_VECTOR_LEN;
        wasm.encodeIamCreateUserRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} group_id
 * @returns {Uint8Array}
 */
export function encodeIamDeleteGroupRequest(group_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(group_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeIamDeleteGroupRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} user_id
 * @returns {Uint8Array}
 */
export function encodeIamDeleteUserRequest(user_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeIamDeleteUserRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} user_id
 * @returns {Uint8Array}
 */
export function encodeIamGetUserRequest(user_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeIamGetUserRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} group_id
 * @returns {Uint8Array}
 */
export function encodeIamGroupMembersRequest(group_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(group_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeIamGroupMembersRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeIamListGroupsRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeIamListGroupsRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} resource_type
 * @param {string} resource_id
 * @returns {Uint8Array}
 */
export function encodeIamListPermsForResourceRequest(resource_type, resource_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(resource_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(resource_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeIamListPermsForResourceRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} subject_type
 * @param {string} subject_id
 * @returns {Uint8Array}
 */
export function encodeIamListPermsForSubjectRequest(subject_type, subject_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeIamListPermsForSubjectRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeIamListUsersRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeIamListUsersRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} user_id
 * @param {string} new_password
 * @returns {Uint8Array}
 */
export function encodeIamResetUserPasswordRequest(user_id, new_password) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(new_password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeIamResetUserPasswordRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} resource_type
 * @param {string} resource_id
 * @param {string} subject_type
 * @param {string} subject_id
 * @param {string} access_level
 * @returns {Uint8Array}
 */
export function encodeIamSetPermissionRequest(resource_type, resource_id, subject_type, subject_id, access_level) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(resource_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(resource_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(subject_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(access_level, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeIamSetPermissionRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} user_id
 * @param {string} group_ids_csv
 * @returns {Uint8Array}
 */
export function encodeIamSetUserGroupsRequest(user_id, group_ids_csv) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(group_ids_csv, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeIamSetUserGroupsRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} group_id
 * @param {string} name
 * @param {string} description
 * @returns {Uint8Array}
 */
export function encodeIamUpdateGroupRequest(group_id, name, description) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(group_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeIamUpdateGroupRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} user_id
 * @param {string} display_name
 * @param {string} email
 * @param {boolean} is_active
 * @param {string} role
 * @returns {Uint8Array}
 */
export function encodeIamUpdateUserRequest(user_id, display_name, email, is_active, role) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(display_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(email, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(role, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeIamUpdateUserRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, is_active, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::LegalAdminBody(GenerateRequest) — render and persist a new
 * RODO/GDPR PDF. `variant` must be one of `short` | `standard` | `full`
 * (server-side validation via `RodoVariant::from_str`).
 * @param {string} variant
 * @returns {Uint8Array}
 */
export function encodeLegalDocumentGenerateRequest(variant) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(variant, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeLegalDocumentGenerateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::LegalAdminBody(RevokeRequest) — soft-delete a previously
 * generated legal document. The PDF stays on disk; the row gets a
 * `revoked_at` stamp and is excluded from default list views.
 * @param {string} doc_id
 * @returns {Uint8Array}
 */
export function encodeLegalDocumentRevokeRequest(doc_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(doc_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeLegalDocumentRevokeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::LegalAdminBody(ListRequest) — fetch the legal documents
 * catalogue. `include_revoked = false` matches the default dashboard view.
 * @param {boolean} include_revoked
 * @returns {Uint8Array}
 */
export function encodeLegalDocumentsListRequest(include_revoked) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeLegalDocumentsListRequest(retptr, include_revoked);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MePreferencesGetRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeMePreferencesGetRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMePreferencesGetRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MePreferencesUpdateRequest { language }.
 * @param {string | null} [language]
 * @returns {Uint8Array}
 */
export function encodeMePreferencesUpdateRequest(language) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(language) ? 0 : passStringToWasm0(language, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeMePreferencesUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {number} item_id
 * @param {string} status
 * @returns {Uint8Array}
 */
export function encodeMeetingActionItemStatusUpdateRequest(item_id, status) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeetingActionItemStatusUpdateRequest(retptr, item_id, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} meeting_key
 * @param {string | null} [status_filter]
 * @returns {Uint8Array}
 */
export function encodeMeetingActionItemsListRequest(meeting_key, status_filter) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(meeting_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(status_filter) ? 0 : passStringToWasm0(status_filter, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeMeetingActionItemsListRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeetingActiveSessionRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeetingActiveSessionRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {number} session_id
 * @param {boolean} include_transcripts
 * @returns {Uint8Array}
 */
export function encodeMeetingSessionDetailRequest(session_id, include_transcripts) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeetingSessionDetailRequest(retptr, session_id, include_transcripts);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {number} session_id
 * @returns {Uint8Array}
 */
export function encodeMeetingSessionLeaveRequest(session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeetingSessionLeaveRequest(retptr, session_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {boolean} only_mine
 * @returns {Uint8Array}
 */
export function encodeMeetingSessionListRequest(only_mine) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeetingSessionListRequest(retptr, only_mine);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} meeting_url
 * @param {string} title
 * @param {string} platform
 * @param {string} bot_name
 * @param {string} stt_alias
 * @param {string} tts_alias
 * @param {string} llm_alias
 * @returns {Uint8Array}
 */
export function encodeMeetingSessionStartRequest(meeting_url, title, platform, bot_name, stt_alias, tts_alias, llm_alias) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(meeting_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(title, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(platform, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(bot_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(stt_alias, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passStringToWasm0(tts_alias, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len5 = WASM_VECTOR_LEN;
        const ptr6 = passStringToWasm0(llm_alias, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len6 = WASM_VECTOR_LEN;
        wasm.encodeMeetingSessionStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v8 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v8;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeetingSettingsGetRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeetingSettingsGetRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `settings` jest JS Array<[key, value]>. Konwertujemy pary do Vec<MeetingSettingKv>.
 * @param {any} settings
 * @returns {Uint8Array}
 */
export function encodeMeetingSettingsUpdateRequest(settings) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeetingSettingsUpdateRequest(retptr, addHeapObject(settings));
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} meeting_key
 * @param {number | null} [limit]
 * @returns {Uint8Array}
 */
export function encodeMeetingSummariesListRequest(meeting_key, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(meeting_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeetingSummariesListRequest(retptr, ptr0, len0, isLikeNone(limit) ? Number.MAX_SAFE_INTEGER : (limit) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} meeting_key
 * @returns {Uint8Array}
 */
export function encodeMeetingTranscriptExportRequest(meeting_key) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(meeting_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeetingTranscriptExportRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {number} session_id
 * @param {number} since_ms
 * @returns {Uint8Array}
 */
export function encodeMeetingTranscriptsListRequest(session_id, since_ms) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeetingTranscriptsListRequest(retptr, session_id, since_ms);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} address
 * @returns {Uint8Array}
 */
export function encodeMeshConnectRequest(address) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(address, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeshConnectRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshIdentityRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeshIdentityRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} node_id
 * @param {string} command
 * @param {string[]} args
 * @returns {Uint8Array}
 */
export function encodeMeshNodeCommandRequest(node_id, command, args) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(command, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayJsValueToWasm0(args, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMeshNodeCommandRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeMeshNodeDetailRequest(node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeshNodeDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshNodeListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeshNodeListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} node_id
 * @param {string} interface_name
 * @param {string} config_json
 * @returns {Uint8Array}
 */
export function encodeMeshNodeNetworkConfigRequest(node_id, interface_name, config_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(interface_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(config_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMeshNodeNetworkConfigRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * `nodeKind` empty and `operator` null both mean "leave this field alone": the
 * dashboard sends only the control the administrator actually moved, and an
 * unchanged field must not travel as an authority statement.
 * @param {string} node_id
 * @param {string} node_kind
 * @param {boolean | null} [operator]
 * @returns {Uint8Array}
 */
export function encodeMeshNodeProfileSetRequest(node_id, node_kind, operator) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node_kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMeshNodeProfileSetRequest(retptr, ptr0, len0, ptr1, len1, isLikeNone(operator) ? 0xFFFFFF : operator ? 1 : 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MeshPairInitRequest { node_id (32 bytes), pin }.
 * @param {Uint8Array} node_id
 * @param {string} pin
 * @returns {Uint8Array}
 */
export function encodeMeshPairInitRequest(node_id, pin) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(node_id, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(pin, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMeshPairInitRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} pair_id
 * @param {string} pin
 * @returns {Uint8Array}
 */
export function encodeMeshPairingConfirmRequest(pair_id, pin) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(pair_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(pin, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMeshPairingConfirmRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} pair_id
 * @returns {Uint8Array}
 */
export function encodeMeshPairingRejectRequest(pair_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(pair_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeshPairingRejectRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} remote_address
 * @param {string | null} [pin_hint]
 * @param {string | null} [remote_public_key]
 * @param {string[] | null} [remote_addresses]
 * @param {string | null} [remote_relay_url]
 * @param {string | null} [remote_hostname]
 * @returns {Uint8Array}
 */
export function encodeMeshPairingStartRequest(remote_address, pin_hint, remote_public_key, remote_addresses, remote_relay_url, remote_hostname) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(remote_address, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(pin_hint) ? 0 : passStringToWasm0(pin_hint, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(remote_public_key) ? 0 : passStringToWasm0(remote_public_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(remote_addresses) ? 0 : passArrayJsValueToWasm0(remote_addresses, wasm.__wbindgen_export);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(remote_relay_url) ? 0 : passStringToWasm0(remote_relay_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(remote_hostname) ? 0 : passStringToWasm0(remote_hostname, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        wasm.encodeMeshPairingStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MeshPeersListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeMeshPeersListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeshPeersListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshPendingListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeshPendingListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshServicesListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeshServicesListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeMeshTrustRetrustRequest(node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeshTrustRetrustRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeMeshTrustRevokeRequest(node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMeshTrustRevokeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshTrustedListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMeshTrustedListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MetaCancelStream (unit variant). Correlation_id idzie w envelope.
 * @returns {Uint8Array}
 */
export function encodeMetaCancelStream() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMetaCancelStream(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MetaHeartbeat { sent_at_epoch }.
 * @param {bigint} sent_at_epoch
 * @returns {Uint8Array}
 */
export function encodeMetaHeartbeat(sent_at_epoch) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMetaHeartbeat(retptr, sent_at_epoch);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MetaSchemaVersionCheck { client_version }.
 * Wysylane raz przy handshake — jesli serwer odrzuci, disconnect.
 * @param {number} client_version
 * @returns {Uint8Array}
 */
export function encodeMetaSchemaVersionCheck(client_version) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMetaSchemaVersionCheck(retptr, client_version);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_id
 * @param {string} attribute
 * @param {string} source_class
 * @param {string} variant
 * @param {string[]} values
 * @param {number} epochs
 * @param {number} batch_size
 * @param {number} learning_rate
 * @param {number} image_size
 * @param {boolean} freeze_backbone
 * @param {string} target_node_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioClassifierTrainStartRequest(project_id, dataset_id, attribute, source_class, variant, values, epochs, batch_size, learning_rate, image_size, freeze_backbone, target_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(attribute, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(source_class, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(variant, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passArrayJsValueToWasm0(values, wasm.__wbindgen_export);
        const len5 = WASM_VECTOR_LEN;
        const ptr6 = passStringToWasm0(target_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len6 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioClassifierTrainStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, epochs, batch_size, learning_rate, image_size, freeze_backbone, ptr6, len6);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v8 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v8;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioDatasetProfileRequest(dataset_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDatasetProfileRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeMlStudioDatasetRowsRequest(dataset_id, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDatasetRowsRequest(retptr, ptr0, len0, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @param {string[]} rows
 * @returns {Uint8Array}
 */
export function encodeMlStudioDatasetRowsSaveRequest(dataset_id, rows) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(rows, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDatasetRowsSaveRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} name
 * @param {string} filename
 * @param {string} upload_id
 * @param {number} seq
 * @param {number} total_chunks
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeMlStudioDatasetUploadChunkRequest(project_id, name, filename, upload_id, seq, total_chunks, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDatasetUploadChunkRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, seq, total_chunks, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Upload a tabular file for profiling. `bytes` arrives from JS as a Uint8Array
 * and wasm-bindgen materializes it directly into `Vec<u8>` — no base64 or copy
 * step on the JS side.
 * @param {string} project_id
 * @param {string} name
 * @param {string} filename
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeMlStudioDatasetUploadRequest(project_id, name, filename, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDatasetUploadRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioDatasetsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDatasetsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_name
 * @param {string} question_source
 * @param {string | null | undefined} source_dataset_id
 * @param {string | null | undefined} question_field
 * @param {string | null | undefined} generate_prompt
 * @param {string | null | undefined} question_model
 * @param {number} num_questions
 * @param {string} teacher_model
 * @param {string | null | undefined} answer_instruction
 * @param {number} temperature
 * @param {number} max_tokens
 * @param {string | null} [objective]
 * @param {string | null} [rejected_model]
 * @param {string | null} [rejected_instruction]
 * @returns {Uint8Array}
 */
export function encodeMlStudioDistillGenerateRequest(project_id, dataset_name, question_source, source_dataset_id, question_field, generate_prompt, question_model, num_questions, teacher_model, answer_instruction, temperature, max_tokens, objective, rejected_model, rejected_instruction) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(question_source, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(source_dataset_id) ? 0 : passStringToWasm0(source_dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(question_field) ? 0 : passStringToWasm0(question_field, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(generate_prompt) ? 0 : passStringToWasm0(generate_prompt, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        var ptr6 = isLikeNone(question_model) ? 0 : passStringToWasm0(question_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len6 = WASM_VECTOR_LEN;
        const ptr7 = passStringToWasm0(teacher_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len7 = WASM_VECTOR_LEN;
        var ptr8 = isLikeNone(answer_instruction) ? 0 : passStringToWasm0(answer_instruction, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len8 = WASM_VECTOR_LEN;
        var ptr9 = isLikeNone(objective) ? 0 : passStringToWasm0(objective, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len9 = WASM_VECTOR_LEN;
        var ptr10 = isLikeNone(rejected_model) ? 0 : passStringToWasm0(rejected_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len10 = WASM_VECTOR_LEN;
        var ptr11 = isLikeNone(rejected_instruction) ? 0 : passStringToWasm0(rejected_instruction, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len11 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDistillGenerateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6, num_questions, ptr7, len7, ptr8, len8, temperature, max_tokens, ptr9, len9, ptr10, len10, ptr11, len11);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v13 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v13;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioDistillGenerateStatusRequest(dataset_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioDistillGenerateStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_id
 * @param {string} message
 * @param {number} max_tokens
 * @returns {Uint8Array}
 */
export function encodeMlStudioFtChatRequest(model_id, message, max_tokens) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(message, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioFtChatRequest(retptr, ptr0, len0, ptr1, len1, max_tokens);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_id
 * @param {string} target_node_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioFtDeployRequest(model_id, target_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioFtDeployRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_id
 * @param {string} outtype
 * @returns {Uint8Array}
 */
export function encodeMlStudioFtExportRequest(model_id, outtype) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(outtype, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioFtExportRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioFtExportStatusRequest(model_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioFtExportStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_id
 * @param {string} base_model
 * @param {string} method
 * @param {string} objective
 * @param {string | null | undefined} teacher_model
 * @param {number} learning_rate
 * @param {number} batch_size
 * @param {number} grad_accum_steps
 * @param {number} epochs
 * @param {number} lora_r
 * @param {number} lora_alpha
 * @param {number} lora_dropout
 * @param {number} max_seq_len
 * @param {boolean} merge_adapter
 * @param {string | null | undefined} target_node_id
 * @param {number} num_gpus
 * @param {number} dist_nnodes
 * @param {number} dist_node_rank
 * @param {string} dist_master_addr
 * @param {number} dist_master_port
 * @returns {Uint8Array}
 */
export function encodeMlStudioFtTrainStartRequest(project_id, dataset_id, base_model, method, objective, teacher_model, learning_rate, batch_size, grad_accum_steps, epochs, lora_r, lora_alpha, lora_dropout, max_seq_len, merge_adapter, target_node_id, num_gpus, dist_nnodes, dist_node_rank, dist_master_addr, dist_master_port) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(base_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(method, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(objective, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(teacher_model) ? 0 : passStringToWasm0(teacher_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        var ptr6 = isLikeNone(target_node_id) ? 0 : passStringToWasm0(target_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len6 = WASM_VECTOR_LEN;
        const ptr7 = passStringToWasm0(dist_master_addr, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len7 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioFtTrainStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, learning_rate, batch_size, grad_accum_steps, epochs, lora_r, lora_alpha, lora_dropout, max_seq_len, merge_adapter, ptr6, len6, num_gpus, dist_nnodes, dist_node_rank, ptr7, len7, dist_master_port);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v9 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v9;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioFtTrainStatusRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioFtTrainStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioGenericTrainStatusRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioGenericTrainStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMlStudioJobsOverviewRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMlStudioJobsOverviewRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dict_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioLookupDictDeleteRequest(dict_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dict_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioLookupDictDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dict_id
 * @param {string} name
 * @param {string} rows_json
 * @returns {Uint8Array}
 */
export function encodeMlStudioLookupDictSaveRequest(project_id, dict_id, name, rows_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dict_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(rows_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioLookupDictSaveRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioLookupDictsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioLookupDictsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioModelsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioModelsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_id
 * @param {string} attribute
 * @param {string} source_class
 * @param {number} epochs
 * @param {number} batch_size
 * @param {number} learning_rate
 * @param {number} synthetic_per_epoch
 * @param {number} real_repeat
 * @param {string} target_node_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioOcrTrainStartRequest(project_id, dataset_id, attribute, source_class, epochs, batch_size, learning_rate, synthetic_per_epoch, real_repeat, target_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(attribute, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(source_class, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(target_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioOcrTrainStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, epochs, batch_size, learning_rate, synthetic_per_epoch, real_repeat, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} name
 * @param {string} description
 * @param {string} project_type
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectCreateRequest(name, description, project_type) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(project_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectDetailRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {boolean} include_models
 * @param {boolean} include_history
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectExportStartRequest(project_id, include_models, include_history) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectExportStartRequest(retptr, ptr0, len0, include_models, include_history);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectExportStatusRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectExportStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectGrantsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectGrantsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} upload_id
 * @param {string} mode
 * @param {string | null} [name_override]
 * @param {string | null} [target_project_id]
 * @param {string | null} [target_dataset_id]
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectImportApplyRequest(upload_id, mode, name_override, target_project_id, target_dataset_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(name_override) ? 0 : passStringToWasm0(name_override, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(target_project_id) ? 0 : passStringToWasm0(target_project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(target_dataset_id) ? 0 : passStringToWasm0(target_dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectImportApplyRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} upload_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectImportCancelRequest(upload_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectImportCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} upload_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectImportPreviewRequest(upload_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectImportPreviewRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectImportStatusRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectImportStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} upload_id
 * @param {number} seq
 * @param {number} total_chunks
 * @param {string} filename
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectImportUploadChunkRequest(upload_id, seq, total_chunks, filename, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectImportUploadChunkRequest(retptr, ptr0, len0, seq, total_chunks, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} upload_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectImportUploadStatusRequest(upload_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectImportUploadStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} invitee_user_id
 * @param {string} role
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectInviteRequest(project_id, invitee_user_id, role) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(invitee_user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(role, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectInviteRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} user_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectMemberRemoveRequest(project_id, user_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectMemberRemoveRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} user_id
 * @param {string} role
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectMemberRoleSetRequest(project_id, user_id, role) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(role, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectMemberRoleSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectMembersListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectMembersListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectResourcesRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioProjectResourcesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectTypesListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMlStudioProjectTypesListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMlStudioProjectsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMlStudioProjectsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @param {number} threshold
 * @param {string} mode
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogAutolabelRequest(dataset_id, threshold, mode) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogAutolabelRequest(retptr, ptr0, len0, threshold, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogAutolabelStatusRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogAutolabelStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_name
 * @param {number} fps
 * @param {string | null} [source_dir]
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogBuildDatasetRequest(project_id, dataset_name, fps, source_dir) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(source_dir) ? 0 : passStringToWasm0(source_dir, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogBuildDatasetRequest(retptr, ptr0, len0, ptr1, len1, fps, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} build_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogBuildStatusRequest(build_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(build_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogBuildStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} name
 * @param {string} path
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogDatasetRegisterRequest(project_id, name, path) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(path, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogDatasetRegisterRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_id
 * @param {number} threshold
 * @param {string} image_b64
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogDetectRequest(model_id, threshold, image_b64) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(image_b64, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogDetectRequest(retptr, ptr0, len0, threshold, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @param {string} image_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogImageRequest(dataset_id, image_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(image_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogImageRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogImagesListRequest(dataset_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogImagesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_id
 * @param {string[]} recording_refs
 * @param {number} fps
 * @param {boolean} autolabel
 * @param {string} collision
 * @param {string | null} [source_node_id]
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogImportRecordingsRequest(project_id, dataset_id, recording_refs, fps, autolabel, collision, source_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayJsValueToWasm0(recording_refs, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(collision, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(source_node_id) ? 0 : passStringToWasm0(source_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogImportRecordingsRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, fps, autolabel, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogImportRecordingsStatusRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogImportRecordingsStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} dataset_id
 * @param {string} image_id
 * @param {string} annotations_json
 * @param {boolean} approve
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogSaveAnnotationsRequest(dataset_id, image_id, annotations_json, approve) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(image_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(annotations_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogSaveAnnotationsRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, approve);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} filename
 * @param {string} upload_id
 * @param {number} seq
 * @param {number} total_chunks
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogStageMediaRequest(project_id, filename, upload_id, seq, total_chunks, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogStageMediaRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, seq, total_chunks, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_id
 * @param {string} variant
 * @param {number} epochs
 * @param {number} batch_size
 * @param {number} grad_accum
 * @param {number} learning_rate
 * @param {number} resolution
 * @param {boolean} early_stopping
 * @param {string | null} [target_node_id]
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogTrainStartRequest(project_id, dataset_id, variant, epochs, batch_size, grad_accum, learning_rate, resolution, early_stopping, target_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(variant, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(target_node_id) ? 0 : passStringToWasm0(target_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogTrainStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, epochs, batch_size, grad_accum, learning_rate, resolution, early_stopping, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecogTrainStatusRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecogTrainStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string | null | undefined} camera_id
 * @param {number | null | undefined} date_from_ms
 * @param {number | null | undefined} date_to_ms
 * @param {number} limit
 * @param {string | null} [source_node_id]
 * @returns {Uint8Array}
 */
export function encodeMlStudioRecordingsListRequest(camera_id, date_from_ms, date_to_ms, limit, source_node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(camera_id) ? 0 : passStringToWasm0(camera_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(source_node_id) ? 0 : passStringToWasm0(source_node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRecordingsListRequest(retptr, ptr0, len0, !isLikeNone(date_from_ms), isLikeNone(date_from_ms) ? 0 : date_from_ms, !isLikeNone(date_to_ms), isLikeNone(date_to_ms) ? 0 : date_to_ms, limit, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} url
 * @param {string} api_key
 * @returns {Uint8Array}
 */
export function encodeMlStudioRemoteImportPreviewRequest(url, api_key) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(api_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRemoteImportPreviewRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} url
 * @param {string} api_key
 * @param {string | null} [name_override]
 * @returns {Uint8Array}
 */
export function encodeMlStudioRemoteImportStartRequest(url, api_key, name_override) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(api_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(name_override) ? 0 : passStringToWasm0(name_override, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRemoteImportStartRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioRemoteImportStatusRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioRemoteImportStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} subject_kind
 * @param {string} subject_id
 * @param {string} node_id
 * @param {string} resource_kind
 * @param {string} resource_ref
 * @param {string} quota
 * @returns {Uint8Array}
 */
export function encodeMlStudioResourceGrantCreateRequest(subject_kind, subject_id, node_id, resource_kind, resource_ref, quota) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(subject_kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(resource_kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(resource_ref, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passStringToWasm0(quota, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len5 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioResourceGrantCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} grant_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioResourceGrantRevokeRequest(grant_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(grant_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioResourceGrantRevokeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMlStudioResourceGrantsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMlStudioResourceGrantsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioSchemaGetRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioSchemaGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} schema_json
 * @returns {Uint8Array}
 */
export function encodeMlStudioSchemaSaveRequest(project_id, schema_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(schema_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioSchemaSaveRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} capability
 * @returns {Uint8Array}
 */
export function encodeMlStudioServiceModelsListRequest(capability) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(capability, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioServiceModelsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @param {string} dataset_id
 * @param {string} target_column
 * @param {string} task
 * @param {string | null} [engine]
 * @returns {Uint8Array}
 */
export function encodeMlStudioTabularTrainRequest(project_id, dataset_id, target_column, task, engine) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(dataset_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(target_column, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(task, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(engine) ? 0 : passStringToWasm0(engine, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioTabularTrainRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioTrainCancelRequest(run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioTrainCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeMlStudioTrainingRunsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioTrainingRunsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_name
 * @returns {Uint8Array}
 */
export function encodeMlStudioVisionModelDeleteRequest(model_name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioVisionModelDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_id
 * @param {string} model_name
 * @param {string} op
 * @param {number | null} [threshold]
 * @param {string | null} [alias]
 * @returns {Uint8Array}
 */
export function encodeMlStudioVisionModelPublishRequest(model_id, model_name, op, threshold, alias) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(model_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(op, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(alias) ? 0 : passStringToWasm0(alias, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeMlStudioVisionModelPublishRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, !isLikeNone(threshold), isLikeNone(threshold) ? 0 : threshold, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeMlStudioVisionModelsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMlStudioVisionModelsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} alias
 * @param {string} target_model
 * @param {string | null} [strategy]
 * @param {string | null} [fallback_targets]
 * @returns {Uint8Array}
 */
export function encodeModelAliasCreateRequest(alias, target_model, strategy, fallback_targets) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(alias, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(strategy) ? 0 : passStringToWasm0(strategy, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(fallback_targets) ? 0 : passStringToWasm0(fallback_targets, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeModelAliasCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {number} id
 * @returns {Uint8Array}
 */
export function encodeModelAliasDeleteRequest(id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeModelAliasDeleteRequest(retptr, id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeModelAliasListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeModelAliasListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {number} id
 * @param {string} alias
 * @param {string} target_model
 * @param {boolean | null} [is_active]
 * @param {string | null} [strategy]
 * @param {string | null} [fallback_targets]
 * @returns {Uint8Array}
 */
export function encodeModelAliasUpdateRequest(id, alias, target_model, is_active, strategy, fallback_targets) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(alias, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(strategy) ? 0 : passStringToWasm0(strategy, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(fallback_targets) ? 0 : passStringToWasm0(fallback_targets, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeModelAliasUpdateRequest(retptr, id, ptr0, len0, ptr1, len1, isLikeNone(is_active) ? 0xFFFFFF : is_active ? 1 : 0, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelConsumerGrantRequest { model_id, addon_id }.
 * @param {string} model_id
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeModelConsumerGrantRequest(model_id, addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeModelConsumerGrantRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelConsumerListRequest { model_id }.
 * @param {string} model_id
 * @returns {Uint8Array}
 */
export function encodeModelConsumerListRequest(model_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeModelConsumerListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelConsumerRevokeRequest { model_id, addon_id }.
 * @param {string} model_id
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeModelConsumerRevokeRequest(model_id, addon_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeModelConsumerRevokeRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelConversionBody(StartRequest) — starts an async TF→ONNX
 * conversion for an existing `services` row (`service_id`).
 * @param {number} service_id
 * @param {string} source_path
 * @param {string} source_format
 * @param {string} precision
 * @param {number} tolerance
 * @param {string | null} [test_input_path]
 * @returns {Uint8Array}
 */
export function encodeModelConversionStartRequest(service_id, source_path, source_format, precision, tolerance, test_input_path) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(source_path, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_format, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(precision, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(test_input_path) ? 0 : passStringToWasm0(test_input_path, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeModelConversionStartRequest(retptr, service_id, ptr0, len0, ptr1, len1, ptr2, len2, tolerance, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelConversionBody(StatusRequest) — polls the last known
 * conversion state for `service_id`.
 * @param {number} service_id
 * @returns {Uint8Array}
 */
export function encodeModelConversionStatusRequest(service_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeModelConversionStatusRequest(retptr, service_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelDeleteRequest { model_id }.
 * @param {string} model_id
 * @returns {Uint8Array}
 */
export function encodeModelDeleteRequest(model_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeModelDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelDetailRequest { model_id }.
 * @param {string} model_id
 * @returns {Uint8Array}
 */
export function encodeModelDetailRequest(model_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeModelDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelInstallRequest { model_id, source_repo }.
 * @param {string} model_id
 * @param {string} source_repo
 * @returns {Uint8Array}
 */
export function encodeModelInstallRequest(model_id, source_repo) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_repo, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeModelInstallRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeModelListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeModelListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} period
 * @param {string} period_key
 * @returns {Uint8Array}
 */
export function encodeModelMetricsNodeServiceRequest(period, period_key) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(period, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(period_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeModelMetricsNodeServiceRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeModelMetricsPricingGet() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeModelMetricsPricingGet(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} model_id
 * @param {number} prompt_per_1k
 * @param {number} completion_per_1k
 * @param {number} audio_per_min
 * @param {number} image_each
 * @param {number} embedding_per_1k
 * @returns {Uint8Array}
 */
export function encodeModelMetricsPricingSet(model_id, prompt_per_1k, completion_per_1k, audio_per_min, image_each, embedding_per_1k) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeModelMetricsPricingSet(retptr, ptr0, len0, prompt_per_1k, completion_per_1k, audio_per_min, image_each, embedding_per_1k);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} period
 * @param {string} period_key
 * @param {string} group_by
 * @param {string | null} [filter_model]
 * @param {string | null} [filter_node]
 * @param {string | null} [filter_service]
 * @param {string | null} [filter_backend]
 * @param {string | null} [filter_modality]
 * @param {string | null} [filter_user]
 * @param {string | null} [filter_group]
 * @returns {Uint8Array}
 */
export function encodeModelMetricsSummaryRequest(period, period_key, group_by, filter_model, filter_node, filter_service, filter_backend, filter_modality, filter_user, filter_group) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(period, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(period_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(group_by, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(filter_model) ? 0 : passStringToWasm0(filter_model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(filter_node) ? 0 : passStringToWasm0(filter_node, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(filter_service) ? 0 : passStringToWasm0(filter_service, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        var ptr6 = isLikeNone(filter_backend) ? 0 : passStringToWasm0(filter_backend, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len6 = WASM_VECTOR_LEN;
        var ptr7 = isLikeNone(filter_modality) ? 0 : passStringToWasm0(filter_modality, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len7 = WASM_VECTOR_LEN;
        var ptr8 = isLikeNone(filter_user) ? 0 : passStringToWasm0(filter_user, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len8 = WASM_VECTOR_LEN;
        var ptr9 = isLikeNone(filter_group) ? 0 : passStringToWasm0(filter_group, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len9 = WASM_VECTOR_LEN;
        wasm.encodeModelMetricsSummaryRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6, ptr7, len7, ptr8, len8, ptr9, len9);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v11 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v11;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelVisibilityListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeModelVisibilityListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeModelVisibilityListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ModelVisibilitySetRequest { model_id, visibility }.
 * @param {string} model_id
 * @param {string} visibility
 * @returns {Uint8Array}
 */
export function encodeModelVisibilitySetRequest(model_id, visibility) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(visibility, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeModelVisibilitySetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::MyOAuthAccountsListRequest (unit) — lista kont biezacego usera.
 * @returns {Uint8Array}
 */
export function encodeMyOAuthAccountsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeMyOAuthAccountsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqConfigGet).
 * @returns {Uint8Array}
 */
export function encodeNetworkConfigGetRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeNetworkConfigGetRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqConfigUpdate(NetworkConfig { .. })).
 * Pola przekazywane jako typed args (no serde-wasm-bindgen); strony JS i WASM
 * zgodne z definicja `NetworkConfig` w `tentaflow-protocol`.
 * @param {string} bind_mode
 * @param {string} bind_ipv4
 * @param {boolean} hide_docker
 * @param {boolean} hide_link_local
 * @param {boolean} hide_loopback
 * @param {boolean} hide_cgnat
 * @param {boolean} prefer_same_subnet
 * @param {string} iroh_relay_url
 * @param {string[]} excluded_interfaces
 * @returns {Uint8Array}
 */
export function encodeNetworkConfigUpdateRequest(bind_mode, bind_ipv4, hide_docker, hide_link_local, hide_loopback, hide_cgnat, prefer_same_subnet, iroh_relay_url, excluded_interfaces) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(bind_mode, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(bind_ipv4, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(iroh_relay_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passArrayJsValueToWasm0(excluded_interfaces, wasm.__wbindgen_export);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeNetworkConfigUpdateRequest(retptr, ptr0, len0, ptr1, len1, hide_docker, hide_link_local, hide_loopback, hide_cgnat, prefer_same_subnet, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqInterfacesList).
 * @returns {Uint8Array}
 */
export function encodeNetworkInterfacesListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeNetworkInterfacesListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqRelayStatus).
 * @returns {Uint8Array}
 */
export function encodeNetworkRelayStatusRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeNetworkRelayStatusRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::NgcStatusRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeNgcStatusRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeNgcStatusRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::NimCatalogListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeNimCatalogListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeNimCatalogListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::PiiRuleBody(ListRequest) — wire-compat z dawnym
 * PiiRuleListRequest, JS API niezmienione.
 * @returns {Uint8Array}
 */
export function encodePiiRuleListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodePiiRuleListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ActiveInfoRequest(..)).
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeProfilingActiveInfoRequest(node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProfilingActiveInfoRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::CollectorsStatusRequest(..)).
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeProfilingCollectorsStatusRequest(node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProfilingCollectorsStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::DeleteRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingDeleteRequest(node_id, session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProfilingDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::DownloadRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingDownloadRequest(node_id, session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProfilingDownloadRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ReportRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingReportRequest(node_id, session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProfilingReportRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::SessionsRequest(..)).
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeProfilingSessionsRequest(node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProfilingSessionsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::StartRequest(..)).
 * @param {string} node_id
 * @param {any} scope
 * @param {string} label
 * @param {string | null} [elevation_password]
 * @returns {Uint8Array}
 */
export function encodeProfilingStartRequest(node_id, scope, label, elevation_password) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(label, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(elevation_password) ? 0 : passStringToWasm0(elevation_password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeProfilingStartRequest(retptr, ptr0, len0, addHeapObject(scope), ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::StopRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingStopRequest(node_id, session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProfilingStopRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ValidateSudoRequest(..)).
 * @param {string} node_id
 * @param {string} password
 * @returns {Uint8Array}
 */
export function encodeProfilingValidateSudoRequest(node_id, password) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(password, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProfilingValidateSudoRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ActivityListRequest). `before_id` crosses
 * the JS boundary as a decimal string (i64 does not fit in a JS number) and
 * is parsed here.
 * @param {string} project_id
 * @param {string | null | undefined} before_id
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioActivityListRequest(project_id, before_id, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(before_id) ? 0 : passStringToWasm0(before_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioActivityListRequest(retptr, ptr0, len0, ptr1, len1, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ApiSpecEndpointsRequest) — parsed endpoint
 * list of an api_spec source.
 * @param {string} project_id
 * @param {string} source_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioApiSpecEndpointsRequest(project_id, source_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioApiSpecEndpointsRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ArchiveStreamRequest) — STREAM-INITIATING (no
 * plain response): chunks = ArchiveStreamChunk, end = ArchiveStreamEnd. Live
 * progress of an export or import job, job owner only.
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioArchiveStreamRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioArchiveStreamRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(AttachmentGetRequest) — download by content
 * hash; the server clamps `max_bytes` to 8 MiB.
 * @param {string} project_id
 * @param {string} sha256
 * @param {number} max_bytes
 * @returns {Uint8Array}
 */
export function encodeProjectStudioAttachmentGetRequest(project_id, sha256, max_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sha256, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioAttachmentGetRequest(retptr, ptr0, len0, ptr1, len1, max_bytes);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(BuildProfileGetRequest).
 * @param {string} project_id
 * @param {string} source_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioBuildProfileGetRequest(project_id, source_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioBuildProfileGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(BuildProfileSaveRequest). `request_json` is
 * the full snake_case field set — upserts the single profile of a source.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioBuildProfileSaveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioBuildProfileSaveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CaseDeleteRequest).
 * @param {string} project_id
 * @param {string} case_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCaseDeleteRequest(project_id, case_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(case_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCaseDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CaseDuplicateRequest).
 * @param {string} project_id
 * @param {string} case_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCaseDuplicateRequest(project_id, case_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(case_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCaseDuplicateRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CaseGetRequest).
 * @param {string} project_id
 * @param {string} case_id
 * @param {boolean} include_versions
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCaseGetRequest(project_id, case_id, include_versions) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(case_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCaseGetRequest(retptr, ptr0, len0, ptr1, len1, include_versions);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CaseRestoreVersionRequest) — restore creates
 * a NEW version (append-only history).
 * @param {string} project_id
 * @param {string} case_id
 * @param {number} version
 * @param {number} expected_version
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCaseRestoreVersionRequest(project_id, case_id, version, expected_version) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(case_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCaseRestoreVersionRequest(retptr, ptr0, len0, ptr1, len1, version, expected_version);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CaseSaveRequest). `request_json` is the full
 * snake_case field set (case_id null = create, expected_version drives
 * optimistic locking).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCaseSaveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCaseSaveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CaseStatusSetRequest) — every downgrade
 * requires `reason`.
 * @param {string} project_id
 * @param {string} case_id
 * @param {string} status
 * @param {string} reason
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCaseStatusSetRequest(project_id, case_id, status, reason) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(case_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(reason, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCaseStatusSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CaseVersionGetRequest).
 * @param {string} project_id
 * @param {string} case_id
 * @param {number} version
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCaseVersionGetRequest(project_id, case_id, version) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(case_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCaseVersionGetRequest(retptr, ptr0, len0, ptr1, len1, version);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CasesBulkStatusRequest). `request_json`
 * carries {project_id, case_ids, status, reason}.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCasesBulkStatusRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCasesBulkStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CasesImportCsvRequest). `request_json`
 * carries {project_id, csv_text, dry_run}.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCasesImportCsvRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCasesImportCsvRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CasesListRequest) — filtered + paged manual
 * test cases; empty filter strings mean "all".
 * @param {string} project_id
 * @param {string} kind
 * @param {string} status
 * @param {string} priority
 * @param {string} tag_id
 * @param {string} origin
 * @param {string} search
 * @param {number} offset
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCasesListRequest(project_id, kind, status, priority, tag_id, origin, search, offset, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(priority, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(tag_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passStringToWasm0(origin, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len5 = WASM_VECTOR_LEN;
        const ptr6 = passStringToWasm0(search, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len6 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCasesListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6, offset, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v8 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v8;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ChatCreateRequest).
 * @param {string} project_id
 * @param {string} title
 * @returns {Uint8Array}
 */
export function encodeProjectStudioChatCreateRequest(project_id, title) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(title, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioChatCreateRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ChatDeleteRequest).
 * @param {string} project_id
 * @param {string} chat_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioChatDeleteRequest(project_id, chat_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(chat_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioChatDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ChatHistoryRequest) — paged, newest first;
 * `before_message_id: None` = latest page.
 * @param {string} project_id
 * @param {string} chat_id
 * @param {string | null | undefined} before_message_id
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioChatHistoryRequest(project_id, chat_id, before_message_id, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(chat_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(before_message_id) ? 0 : passStringToWasm0(before_message_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioChatHistoryRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ChatRenameRequest).
 * @param {string} project_id
 * @param {string} chat_id
 * @param {string} title
 * @returns {Uint8Array}
 */
export function encodeProjectStudioChatRenameRequest(project_id, chat_id, title) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(chat_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(title, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioChatRenameRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ChatStreamRequest) — send one user message
 * and stream the assistant reply (chunks = ChatStreamChunk, end =
 * ChatStreamEnd).
 * @param {string} project_id
 * @param {string} chat_id
 * @param {string} message
 * @returns {Uint8Array}
 */
export function encodeProjectStudioChatStreamRequest(project_id, chat_id, message) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(chat_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(message, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioChatStreamRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ChatsListRequest) — caller's own chats only.
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioChatsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioChatsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CodeAssistRequest) — STREAM-INITIATING:
 * tokens stream via CodeAssistStreamChunk, the final proposal arrives whole
 * in CodeAssistStreamEnd.
 * @param {string} project_id
 * @param {string} case_id
 * @param {string} kind
 * @param {string} selection
 * @param {string} instruction
 * @param {string} full_content
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCodeAssistRequest(project_id, case_id, kind, selection, instruction, full_content) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(case_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(selection, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(instruction, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passStringToWasm0(full_content, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len5 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCodeAssistRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CreatorGrantSetRequest) — admin only.
 * @param {string} user_id
 * @param {boolean} granted
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCreatorGrantSetRequest(user_id, granted) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioCreatorGrantSetRequest(retptr, ptr0, len0, granted);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(CreatorGrantsListRequest) — admin only.
 * @returns {Uint8Array}
 */
export function encodeProjectStudioCreatorGrantsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeProjectStudioCreatorGrantsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(EnvApprovalDecideRequest) — admin only,
 * rejection requires a non-empty `reason`.
 * @param {string} project_id
 * @param {string} environment_id
 * @param {boolean} approve
 * @param {string} reason
 * @returns {Uint8Array}
 */
export function encodeProjectStudioEnvApprovalDecideRequest(project_id, environment_id, approve, reason) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(environment_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(reason, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioEnvApprovalDecideRequest(retptr, ptr0, len0, ptr1, len1, approve, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(EnvApprovalsListRequest) — admin only,
 * cross-project pending environment approvals.
 * @returns {Uint8Array}
 */
export function encodeProjectStudioEnvApprovalsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeProjectStudioEnvApprovalsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(EnvironmentDeleteRequest).
 * @param {string} project_id
 * @param {string} environment_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioEnvironmentDeleteRequest(project_id, environment_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(environment_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioEnvironmentDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(EnvironmentSaveRequest). `request_json` is
 * the full snake_case field set (environment_id null = create; secret null =
 * keep stored, "" = clear, value = replace).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioEnvironmentSaveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioEnvironmentSaveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(EnvironmentsListRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioEnvironmentsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioEnvironmentsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(GenerationCancelRequest).
 * @param {string} project_id
 * @param {string} gen_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioGenerationCancelRequest(project_id, gen_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(gen_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioGenerationCancelRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(GenerationDeleteRequest).
 * @param {string} project_id
 * @param {string} gen_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioGenerationDeleteRequest(project_id, gen_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(gen_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioGenerationDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(GenerationGetRequest) — polling is the
 * source of truth for generation progress.
 * @param {string} project_id
 * @param {string} gen_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioGenerationGetRequest(project_id, gen_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(gen_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioGenerationGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(GenerationReviewRequest). `request_json`
 * carries {project_id, gen_id, accept_case_ids, reject_case_ids}.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioGenerationReviewRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioGenerationReviewRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(GenerationStartRequest). `request_json`
 * carries {project_id, kind, source_ids, requested_count, instructions,
 * agent_id?} — agent_id null = the project's 'generator_manual' binding.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioGenerationStartRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioGenerationStartRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(GenerationsListRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioGenerationsListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioGenerationsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(IngestCancelRequest).
 * @param {string} project_id
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioIngestCancelRequest(project_id, job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioIngestCancelRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(IngestStatusRequest) — polling is the source
 * of truth for job state; the stream is only a live view.
 * @param {string} project_id
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioIngestStatusRequest(project_id, job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioIngestStatusRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(IngestStreamRequest) — subscribe to live
 * ingest logs/progress (chunks = IngestStreamChunk, end = IngestStreamEnd).
 * @param {string} project_id
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioIngestStreamRequest(project_id, job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioIngestStreamRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(KbSearchRequest). `source_ids_json` is a
 * JSON array of source ids ([] = search all sources).
 * @param {string} project_id
 * @param {string} query
 * @param {string} source_ids_json
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioKbSearchRequest(project_id, query, source_ids_json, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(source_ids_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioKbSearchRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MemberCandidatesRequest). `project_id: None`
 * = creation wizard (grant holders), `Some` = the "Invite" modal (manager+).
 * @param {string | null | undefined} project_id
 * @param {string} query
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMemberCandidatesRequest(project_id, query, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(project_id) ? 0 : passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMemberCandidatesRequest(retptr, ptr0, len0, ptr1, len1, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MemberRemoveRequest).
 * @param {string} project_id
 * @param {string} user_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMemberRemoveRequest(project_id, user_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMemberRemoveRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MemberRoleSetRequest).
 * @param {string} project_id
 * @param {string} user_id
 * @param {string} role
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMemberRoleSetRequest(project_id, user_id, role) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(role, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMemberRoleSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MembersAddRequest). `members_json` is a JSON
 * array of MemberInputWire objects ({user_id, role}).
 * @param {string} project_id
 * @param {string} members_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMembersAddRequest(project_id, members_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(members_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMembersAddRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MembersListRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMembersListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMembersListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MlLinkAttachRequest). `request_json` carries
 * the role_map array.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMlLinkAttachRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMlLinkAttachRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MlLinkDetachRequest) — `revoke_members` also
 * removes the ML memberships this link granted; the ML project survives.
 * @param {string} project_id
 * @param {string} link_id
 * @param {boolean} revoke_members
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMlLinkDetachRequest(project_id, link_id, revoke_members) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(link_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMlLinkDetachRequest(retptr, ptr0, len0, ptr1, len1, revoke_members);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MlLinkSyncNowRequest).
 * @param {string} project_id
 * @param {string} link_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMlLinkSyncNowRequest(project_id, link_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(link_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMlLinkSyncNowRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MlLinkUpdateRequest). `request_json` carries
 * the role_map array.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMlLinkUpdateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMlLinkUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MlLinksListRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMlLinksListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMlLinksListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MlProjectCandidatesRequest) — ML projects the
 * caller OWNS and that are not linked yet.
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMlProjectCandidatesRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMlProjectCandidatesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MlProjectCreateFromProjectRequest).
 * `request_json` carries the role_map array, so it goes through serde like
 * the other multi-field mutations.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMlProjectCreateFromProjectRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioMlProjectCreateFromProjectRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(MyTestWorkRequest) — cross-project, no
 * project_id (aggregates the caller's open run items).
 * @returns {Uint8Array}
 */
export function encodeProjectStudioMyTestWorkRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeProjectStudioMyTestWorkRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(NotificationsListRequest) — always
 * caller-scoped, no project_id.
 * @param {boolean} only_unread
 * @param {string | null | undefined} before_id
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioNotificationsListRequest(only_unread, before_id, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(before_id) ? 0 : passStringToWasm0(before_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioNotificationsListRequest(retptr, only_unread, ptr0, len0, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(NotificationsMarkReadRequest).
 * `request_json` carries {notification_ids} — empty = mark ALL as read.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioNotificationsMarkReadRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioNotificationsMarkReadRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(OverviewRequest) — KPIs + recent activity.
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioOverviewRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioOverviewRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(OwnershipTransferRequest).
 * @param {string} project_id
 * @param {string} new_owner_user_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioOwnershipTransferRequest(project_id, new_owner_user_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(new_owner_user_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioOwnershipTransferRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectArchiveRequest) — archive/unarchive.
 * @param {string} project_id
 * @param {boolean} archived
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectArchiveRequest(project_id, archived) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectArchiveRequest(retptr, ptr0, len0, archived);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectCreateRequest). `modules_json` is a
 * JSON array of module names; `members_json` is a JSON array of
 * MemberInputWire objects ({user_id, role}).
 * @param {string} name
 * @param {string} description
 * @param {string} template
 * @param {string} modules_json
 * @param {string} members_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectCreateRequest(name, description, template, modules_json, members_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(template, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(modules_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(members_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectDeleteRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectDeleteRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectExportStartRequest). `request_json`
 * carries the include_* switches; `include_user_names` copies display names
 * into the archive (personal data — audited).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectExportStartRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectExportStartRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectExportStatusRequest) — polling this is
 * the source of truth; ArchiveStream is only a live view.
 * @param {string} project_id
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectExportStatusRequest(project_id, job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectExportStatusRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectGetRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectGetRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectImportApplyRequest). `request_json`
 * carries the upload_id plus the name override and import switches.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectImportApplyRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectImportApplyRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectImportPreviewRequest) — reads ONLY the
 * archive manifest; nothing is unpacked before the user confirms.
 * @param {string} upload_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectImportPreviewRequest(upload_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectImportPreviewRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectImportStatusRequest) — addressed by
 * job_id alone: the project row does not exist until the import succeeds.
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectImportStatusRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectImportStatusRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectImportUploadChunkRequest) — one raw
 * chunk of an uploaded archive (`bytes` is a Uint8Array on the JS side);
 * `upload_id` is client-minted and there is no project_id yet.
 * @param {string} upload_id
 * @param {string} filename
 * @param {number} seq
 * @param {number} total_chunks
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectImportUploadChunkRequest(upload_id, filename, seq, total_chunks, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectImportUploadChunkRequest(retptr, ptr0, len0, ptr1, len1, seq, total_chunks, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectUpdateRequest) — name/description.
 * @param {string} project_id
 * @param {string} name
 * @param {string} description
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectUpdateRequest(project_id, name, description) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioProjectUpdateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ProjectsListRequest) — project registry.
 * @param {boolean} include_archived
 * @returns {Uint8Array}
 */
export function encodeProjectStudioProjectsListRequest(include_archived) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeProjectStudioProjectsListRequest(retptr, include_archived);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ReportQueryRequest). `request_json` carries
 * {project_id, report, from_date, to_date, suite_id} — one generic variant,
 * rows_json schema is per report.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioReportQueryRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioReportQueryRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunArtifactGetRequest) — artifact download
 * by id; the server clamps `max_bytes` to 32 MiB, response.bytes arrives as
 * a Uint8Array.
 * @param {string} project_id
 * @param {string} artifact_id
 * @param {number} max_bytes
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunArtifactGetRequest(project_id, artifact_id, max_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(artifact_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunArtifactGetRequest(retptr, ptr0, len0, ptr1, len1, max_bytes);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunAutoCancelRequest).
 * @param {string} project_id
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunAutoCancelRequest(project_id, run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunAutoCancelRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunAutoGetRequest) — polling snapshot, the
 * source of truth for automated-run progress.
 * @param {string} project_id
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunAutoGetRequest(project_id, run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunAutoGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunAutoStreamRequest) — subscribe to the
 * live view of an automated run (chunks = RunAutoStreamChunk, end =
 * RunAutoStreamEnd).
 * @param {string} project_id
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunAutoStreamRequest(project_id, run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunAutoStreamRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunCloseRequest) — `cancelled` = abort
 * instead of complete.
 * @param {string} project_id
 * @param {string} run_id
 * @param {boolean} cancelled
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunCloseRequest(project_id, run_id, cancelled) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunCloseRequest(retptr, ptr0, len0, ptr1, len1, cancelled);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunCreateRequest). `request_json` carries
 * the full field set; exactly ONE of suite_id / case_ids / from_failed_run_id
 * selects the case source (server-enforced).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunDeleteRequest).
 * @param {string} project_id
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunDeleteRequest(project_id, run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunGetRequest).
 * @param {string} project_id
 * @param {string} run_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunGetRequest(project_id, run_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunItemClaimRequest). `item_id: None` claims
 * the nearest pool item atomically server-side.
 * @param {string} project_id
 * @param {string} run_id
 * @param {string | null} [item_id]
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunItemClaimRequest(project_id, run_id, item_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(run_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(item_id) ? 0 : passStringToWasm0(item_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunItemClaimRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunItemFinishRequest). `request_json`
 * carries the full field set; empty status = server derives the verdict.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunItemFinishRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunItemFinishRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunItemGetRequest).
 * @param {string} project_id
 * @param {string} item_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunItemGetRequest(project_id, item_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(item_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunItemGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunItemReleaseRequest).
 * @param {string} project_id
 * @param {string} item_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunItemReleaseRequest(project_id, item_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(item_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunItemReleaseRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunStartAutoRequest). `request_json` carries
 * the XOR case selectors (suite_id / case_ids / from_run_id), environment_id,
 * optional runner_service_id and perf_profile_json.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunStartAutoRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunStartAutoRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunStepSetRequest). `request_json` carries
 * {project_id, item_id, step_index, status, note, attachments_json}.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunStepSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunStepSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunnersListRequest) — test-runner discovery.
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunnersListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunnersListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(RunsListRequest) — filtered + paged runs.
 * @param {string} project_id
 * @param {string} status
 * @param {string} run_type
 * @param {number} offset
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioRunsListRequest(project_id, status, run_type, offset, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(run_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioRunsListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, offset, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ScheduleDeleteRequest).
 * @param {string} project_id
 * @param {string} schedule_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioScheduleDeleteRequest(project_id, schedule_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(schedule_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioScheduleDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ScheduleRunNowRequest) — fires through the
 * same gate chain as the loop and never moves `next_run_at`.
 * @param {string} project_id
 * @param {string} schedule_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioScheduleRunNowRequest(project_id, schedule_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(schedule_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioScheduleRunNowRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ScheduleRunsListRequest) — trigger history
 * of one schedule.
 * @param {string} project_id
 * @param {string} schedule_id
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeProjectStudioScheduleRunsListRequest(project_id, schedule_id, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(schedule_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioScheduleRunsListRequest(retptr, ptr0, len0, ptr1, len1, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ScheduleSaveRequest). `request_json` is the
 * COMPLETE definition (schedule_id null = create); every omitted field is a
 * real clear, so list-row toggles must use ScheduleSetEnabledRequest instead.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioScheduleSaveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioScheduleSaveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(ScheduleSetEnabledRequest) — enable/disable
 * toggle that leaves the rest of the definition untouched.
 * @param {string} project_id
 * @param {string} schedule_id
 * @param {boolean} enabled
 * @returns {Uint8Array}
 */
export function encodeProjectStudioScheduleSetEnabledRequest(project_id, schedule_id, enabled) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(schedule_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioScheduleSetEnabledRequest(retptr, ptr0, len0, ptr1, len1, enabled);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SchedulesListRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSchedulesListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSchedulesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SettingsGetRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSettingsGetRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSettingsGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SettingsSaveRequest) — partial save; `None`
 * fields are left untouched. `agents_json` is a JSON array of
 * ProjectAgentBinding objects; `modules_json` is a JSON array of module names
 * that REPLACES the enabled set (absent, empty or `null` = leave untouched).
 * `graph_extraction` is the per-project knowledge-graph toggle; `None` leaves
 * it alone, so a basics or modules save cannot flip it by omission.
 * @param {string} project_id
 * @param {string | null} [name]
 * @param {string | null} [description]
 * @param {string | null} [agents_json]
 * @param {string | null} [modules_json]
 * @param {boolean | null} [graph_extraction]
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSettingsSaveRequest(project_id, name, description, agents_json, modules_json, graph_extraction) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(name) ? 0 : passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(agents_json) ? 0 : passStringToWasm0(agents_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(modules_json) ? 0 : passStringToWasm0(modules_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSettingsSaveRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, isLikeNone(graph_extraction) ? 0xFFFFFF : graph_extraction ? 1 : 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceCreateRequest). `config_json` passes
 * through as-is (server validates per kind); `file_refs_json` is a JSON array
 * of sha256 refs returned by the upload.
 * @param {string} project_id
 * @param {string} kind
 * @param {string} name
 * @param {string} config_json
 * @param {string} file_refs_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceCreateRequest(project_id, kind, name, config_json, file_refs_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(config_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(file_refs_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceDeleteRequest).
 * @param {string} project_id
 * @param {string} source_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceDeleteRequest(project_id, source_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceFileDeleteRequest).
 * @param {string} project_id
 * @param {string} file_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceFileDeleteRequest(project_id, file_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(file_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceFileDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceFilePreviewRequest) — text-only
 * preview; the server clamps `max_bytes`.
 * @param {string} project_id
 * @param {string} file_id
 * @param {number} max_bytes
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceFilePreviewRequest(project_id, file_id, max_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(file_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceFilePreviewRequest(retptr, ptr0, len0, ptr1, len1, max_bytes);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceFilesListRequest) — paged file list.
 * @param {string} project_id
 * @param {string} source_id
 * @param {number} offset
 * @param {number} limit
 * @param {string} filter
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceFilesListRequest(project_id, source_id, offset, limit, filter) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(filter, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceFilesListRequest(retptr, ptr0, len0, ptr1, len1, offset, limit, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceRefreshRequest) — git sources only:
 * fetch + delta re-index of changed files.
 * @param {string} project_id
 * @param {string} source_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceRefreshRequest(project_id, source_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceRefreshRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceReingestRequest). `file_id: Some` =
 * re-ingest a single file, `None` = the whole source.
 * @param {string} project_id
 * @param {string} source_id
 * @param {string | null} [file_id]
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceReingestRequest(project_id, source_id, file_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(file_id) ? 0 : passStringToWasm0(file_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceReingestRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceSecretSetRequest) — sets/clears the
 * access token of a git source; `token` None clears (input-only, reads never
 * return it).
 * @param {string} project_id
 * @param {string} source_id
 * @param {string | null} [token]
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceSecretSetRequest(project_id, source_id, token) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(token) ? 0 : passStringToWasm0(token, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceSecretSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceUpdateRequest).
 * @param {string} project_id
 * @param {string} source_id
 * @param {string} name
 * @param {string} config_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceUpdateRequest(project_id, source_id, name, config_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(config_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceUpdateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourceUploadChunkRequest) — one raw chunk of
 * an uploaded file (`bytes` is a Uint8Array on the JS side).
 * @param {string} project_id
 * @param {string} upload_id
 * @param {string} filename
 * @param {string} mime
 * @param {number} seq
 * @param {number} total_chunks
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourceUploadChunkRequest(project_id, upload_id, filename, mime, seq, total_chunks, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(mime, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourceUploadChunkRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, seq, total_chunks, ptr4, len4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SourcesListRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSourcesListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSourcesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SuiteDeleteRequest).
 * @param {string} project_id
 * @param {string} suite_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSuiteDeleteRequest(project_id, suite_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(suite_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSuiteDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SuiteGetRequest).
 * @param {string} project_id
 * @param {string} suite_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSuiteGetRequest(project_id, suite_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(suite_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSuiteGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SuiteSaveRequest). `request_json` carries
 * {project_id, suite_id?, name, description, case_ids} — case order defines
 * member positions.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSuiteSaveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSuiteSaveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(SuitesListRequest).
 * @param {string} project_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioSuitesListRequest(project_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioSuitesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TagDeleteRequest).
 * @param {string} project_id
 * @param {string} tag_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTagDeleteRequest(project_id, tag_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(tag_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTagDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TagSaveRequest). `tag_id: None` = create,
 * `Some` = rename.
 * @param {string} project_id
 * @param {string | null | undefined} tag_id
 * @param {string} name
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTagSaveRequest(project_id, tag_id, name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(tag_id) ? 0 : passStringToWasm0(tag_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTagSaveRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TaskCommentAddRequest).
 * @param {string} project_id
 * @param {string} task_id
 * @param {string} body_md
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTaskCommentAddRequest(project_id, task_id, body_md) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(task_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(body_md, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTaskCommentAddRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TaskCommentDeleteRequest).
 * @param {string} project_id
 * @param {string} comment_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTaskCommentDeleteRequest(project_id, comment_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(comment_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTaskCommentDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TaskCommentEditRequest).
 * @param {string} project_id
 * @param {string} comment_id
 * @param {string} body_md
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTaskCommentEditRequest(project_id, comment_id, body_md) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(comment_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(body_md, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTaskCommentEditRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TaskDeleteRequest).
 * @param {string} project_id
 * @param {string} task_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTaskDeleteRequest(project_id, task_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(task_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTaskDeleteRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TaskGetRequest).
 * @param {string} project_id
 * @param {string} task_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTaskGetRequest(project_id, task_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(task_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTaskGetRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TaskSaveRequest). `request_json` is the full
 * snake_case field set (task_id null = create; defects require severity).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTaskSaveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTaskSaveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TaskStatusSetRequest) — status-only kanban
 * move; TaskSaveRequest would clear description_md and attachments.
 * @param {string} project_id
 * @param {string} task_id
 * @param {string} status
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTaskStatusSetRequest(project_id, task_id, status) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(task_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTaskStatusSetRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TasksListRequest) — filtered + paged tasks
 * and defects.
 * @param {string} project_id
 * @param {string} task_type
 * @param {string} status
 * @param {string} assigned_to
 * @param {string} search
 * @param {number} offset
 * @param {number} limit
 * @param {string | null} [severity]
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTasksListRequest(project_id, task_type, status, assigned_to, search, offset, limit, severity) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(task_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(assigned_to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(search, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(severity) ? 0 : passStringToWasm0(severity, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTasksListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, offset, limit, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TryRunCancelRequest) — addresses the
 * ephemeral execution by the client-minted try_id.
 * @param {string} project_id
 * @param {string} try_id
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTryRunCancelRequest(project_id, try_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(try_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTryRunCancelRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ProjectStudioBody(TryRunStartRequest) — STREAM-INITIATING (no
 * plain response): chunks = TryRunStreamChunk, end = TryRunStreamEnd on the
 * same subscription. `request_json` carries the client-minted try_id plus
 * case/environment/override fields.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeProjectStudioTryRunStartRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeProjectStudioTryRunStartRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::PromptDetailRequest { prompt_id }.
 * @param {string} prompt_id
 * @returns {Uint8Array}
 */
export function encodePromptDetailRequest(prompt_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(prompt_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodePromptDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::PromptListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodePromptListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodePromptListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RegistryListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeRegistryListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeRegistryListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RerankBody(Request) — encoder rerankingu (Tier 1). `topN`
 * opcjonalne (None = wszystkie dokumenty).
 * @param {string} model
 * @param {string} query
 * @param {string[]} documents
 * @param {number | null | undefined} top_n
 * @param {boolean} return_documents
 * @returns {Uint8Array}
 */
export function encodeRerankRequest(model, query, documents, top_n, return_documents) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArrayJsValueToWasm0(documents, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeRerankRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, isLikeNone(top_n) ? Number.MAX_SAFE_INTEGER : (top_n) >>> 0, return_documents);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RobotsBody(CameraShareRequest) — expose a robot's camera to
 * TentaVision (local grant) or surface the remote-view note (remote robot).
 * @param {string} robot_id
 * @param {string} camera_id
 * @returns {Uint8Array}
 */
export function encodeRobotCameraShareRequest(robot_id, camera_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(robot_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(camera_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeRobotCameraShareRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RobotsBody(ControlRequest) — route a typed, allowlisted action to
 * the robot's owning node. The `vx`/`vy`/`vyaw` axes apply to "move" only; the
 * `p1..p4` generic params carry parametered poses/levels keyed by `kind` (euler
 * → roll/pitch/yaw; body_height/foot_raise_height → p1=height; speed_level →
 * p1=level; pose → roll/pitch/yaw/height). The owner clamps every numeric param
 * to the documented Go2 range.
 * @param {string} robot_id
 * @param {string} kind
 * @param {number} vx
 * @param {number} vy
 * @param {number} vyaw
 * @param {number} p1
 * @param {number} p2
 * @param {number} p3
 * @param {number} p4
 * @returns {Uint8Array}
 */
export function encodeRobotControlRequest(robot_id, kind, vx, vy, vyaw, p1, p2, p3, p4) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(robot_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeRobotControlRequest(retptr, ptr0, len0, ptr1, len1, vx, vy, vyaw, p1, p2, p3, p4);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RobotsBody(GeoAnchorGetRequest) — read a robot's geo anchor + live
 * real-world position.
 * @param {string} robot_id
 * @returns {Uint8Array}
 */
export function encodeRobotGeoAnchorGetRequest(robot_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(robot_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRobotGeoAnchorGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RobotsBody(GeoAnchorSetRequest) — pin the robot's scene origin to a
 * real-world lat/lon/alt + heading (all `Some` = set; all `None` = clear).
 * @param {string} robot_id
 * @param {number | null} [lat]
 * @param {number | null} [lon]
 * @param {number | null} [alt]
 * @param {number | null} [heading]
 * @returns {Uint8Array}
 */
export function encodeRobotGeoAnchorSetRequest(robot_id, lat, lon, alt, heading) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(robot_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRobotGeoAnchorSetRequest(retptr, ptr0, len0, !isLikeNone(lat), isLikeNone(lat) ? 0 : lat, !isLikeNone(lon), isLikeNone(lon) ? 0 : lon, !isLikeNone(alt), isLikeNone(alt) ? 0 : alt, !isLikeNone(heading), isLikeNone(heading) ? 0 : heading);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RobotsBody(ListRequest) — org-scoped robot list.
 * @returns {Uint8Array}
 */
export function encodeRobotsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeRobotsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RoleCatalogBody(CreateRequest) — payload jako JSON object
 * odpowiadajacy `RoleCatalogCreateRequest`. Translations sa parami
 * `[code, value]`; brak ikony / color_hint w obiekcie = None.
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeRoleCatalogCreateRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRoleCatalogCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RoleCatalogBody(DeactivateRequest { id }).
 * @param {string} id
 * @returns {Uint8Array}
 */
export function encodeRoleCatalogDeactivateRequest(id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRoleCatalogDeactivateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RoleCatalogBody(GetBySlugRequest { slug }).
 * @param {string} slug
 * @returns {Uint8Array}
 */
export function encodeRoleCatalogGetBySlugRequest(slug) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(slug, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRoleCatalogGetBySlugRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RoleCatalogBody(GetRequest { id }).
 * @param {string} id
 * @returns {Uint8Array}
 */
export function encodeRoleCatalogGetRequest(id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRoleCatalogGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RoleCatalogBody(ListLocalesRequest) — unit variant.
 * @returns {Uint8Array}
 */
export function encodeRoleCatalogListLocalesRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeRoleCatalogListLocalesRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RoleCatalogBody(ListRequest) — filter jako JSON object.
 * Wszystkie pola filter opcjonalne; pusty `{}` zwraca pelna liste.
 * @param {string} filter_json
 * @returns {Uint8Array}
 */
export function encodeRoleCatalogListRequest(filter_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(filter_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRoleCatalogListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::RoleCatalogBody(UpdateRequest) — patch update.
 * `Option<Option<String>>` w JSON: brak pola = nie ruszaj, `null` = wyzeruj,
 * string = ustaw. Vec<(String, String)> jako lista par `[["pl","..."], ...]`.
 * Serde nie potrafi rozroznic "missing" od "null" dla `Option<Option<T>>`,
 * wiec parsujemy ręcznie z `serde_json::Value`.
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeRoleCatalogUpdateRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeRoleCatalogUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeSchedulerActionsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeSchedulerActionsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeSchedulerJobDeleteRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSchedulerJobDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @returns {Uint8Array}
 */
export function encodeSchedulerJobRunNowRequest(job_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSchedulerJobRunNowRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_json
 * @returns {Uint8Array}
 */
export function encodeSchedulerJobUpsertRequest(job_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSchedulerJobUpsertRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeSchedulerJobsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeSchedulerJobsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} job_id
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeSchedulerRunsListRequest(job_id, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(job_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSchedulerRunsListRequest(retptr, ptr0, len0, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqAgent) — execute a validated
 * operation against a node-owned Codex or Claude Code bridge.
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeServiceAgentRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceAgentRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqUpdate) — edycja serwisu po
 * deploy (Edit modal). 13 pól opcjonalnych; klient sam decyduje co jest
 * `Some(_)`. Payload przyjmujemy jako JSON string żeby nie trzymać 13
 * argumentów wasm-bindgen.
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeServiceConfigUpdateRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceConfigUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqDelete) — stop + delete the row
 * (cascades to `model_registry`).
 * @param {number} service_id
 * @param {string | null} [node_id]
 * @returns {Uint8Array}
 */
export function encodeServiceDeleteRequest(service_id, node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceDeleteRequest(retptr, service_id, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqEnginePresets) — lista
 * presetów modelu z manifestu silnika (single source of truth z
 * `tentaflow-containers/<cat>/_services/<engine>.toml`).
 * @param {string} engine_id
 * @returns {Uint8Array}
 */
export function encodeServiceEnginePresetsRequest(engine_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(engine_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceEnginePresetsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqList). Empty filter values are
 * treated as "no filter".
 * @param {string | null} [engine_id_filter]
 * @param {string | null} [category_filter]
 * @returns {Uint8Array}
 */
export function encodeServiceListRequest(engine_id_filter, category_filter) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(engine_id_filter) ? 0 : passStringToWasm0(engine_id_filter, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(category_filter) ? 0 : passStringToWasm0(category_filter, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeServiceListRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::DeploymentBody(ReqStart) — inicjuje deploy silnika z manifestu.
 * `config_json` przyjmujemy jako stringify JSON z GUI (elastyczna struktura).
 * Nazwa wasm-bindgen `encodeServiceManifestDeployRequest` zachowana dla
 * kompatybilności z frontend codec.js — pod spodem opakowujemy w
 * DeploymentBody::ReqStart (po konsolidacji na inner enum).
 * @param {string} engine_id
 * @param {string} deploy_method
 * @param {string} node_id
 * @param {string} config_json
 * @returns {Uint8Array}
 */
export function encodeServiceManifestDeployRequest(engine_id, deploy_method, node_id, config_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(engine_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(deploy_method, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(config_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeServiceManifestDeployRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqModelCatalog) — live model
 * catalog of a deployed external provider service (fetched from provider API).
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeServiceModelCatalogRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceModelCatalogRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqModelSelection) — persist the
 * admin's model selection (model_registry upserted to exactly this set).
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeServiceModelSelectionRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceModelSelectionRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqOauthPoll) — poll a login
 * flow's status.
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeServiceOauthPollRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceOauthPollRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqOauthStart) — begin a
 * subscription OAuth login (browser PKCE) on the named node.
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeServiceOauthStartRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceOauthStartRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqPause) — supervisor leaves a
 * paused service untouched.
 * @param {number} service_id
 * @param {boolean} paused
 * @param {string | null} [node_id]
 * @returns {Uint8Array}
 */
export function encodeServicePauseRequest(service_id, paused, node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeServicePauseRequest(retptr, service_id, paused, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqPin) — toggles the pin flag
 * used by the supervisor for auto-respawn.
 * @param {number} service_id
 * @param {boolean} pinned
 * @param {string | null} [node_id]
 * @returns {Uint8Array}
 */
export function encodeServicePinRequest(service_id, pinned, node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeServicePinRequest(retptr, service_id, pinned, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Redeploy in-place: backend reużywa zapisany `config_json` serwisu, więc
 * frontend wysyła tylko `service_id`.
 * @param {number} service_id
 * @returns {Uint8Array}
 */
export function encodeServiceRedeployRequest(service_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeServiceRedeployRequest(retptr, service_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqStart) — unpause + spawn the
 * engine when stopped/failed/paused. Idempotent for already-running services.
 * @param {number} service_id
 * @param {string | null} [node_id]
 * @returns {Uint8Array}
 */
export function encodeServiceStartRequest(service_id, node_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceStartRequest(retptr, service_id, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqVramHint) — snapshot VRAM
 * per GPU + lista zewnętrznych procesów (sunshine, chrome itp.).
 * @param {number | null} [gpu_index]
 * @param {string | null} [node_id]
 * @param {number | null} [exclude_service_id]
 * @returns {Uint8Array}
 */
export function encodeServiceVramHintRequest(gpu_index, node_id, exclude_service_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        wasm.encodeServiceVramHintRequest(retptr, isLikeNone(gpu_index) ? Number.MAX_SAFE_INTEGER : (gpu_index) >>> 0, ptr0, len0, !isLikeNone(exclude_service_id), isLikeNone(exclude_service_id) ? 0 : exclude_service_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::SettingsListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeSettingsListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeSettingsListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::SettingsUpdateRequest — trzy rownolegle tablice (keys/values/is_secrets).
 * Wszystkie 3 musza miec ten sam dlugosc. Pozwala na batch update z JS bez
 * serde-wasm-bindgen.
 * @param {string[]} keys
 * @param {string[]} values
 * @param {Uint8Array} is_secrets
 * @returns {Uint8Array}
 */
export function encodeSettingsUpdateBatch(keys, values, is_secrets) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArrayJsValueToWasm0(keys, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(values, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passArray8ToWasm0(is_secrets, wasm.__wbindgen_export);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeSettingsUpdateBatch(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
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
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(value, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeSettingsUpdateSingle(retptr, ptr0, len0, ptr1, len1, is_secret);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} snapshot_id
 * @param {string} approved_actions_json
 * @returns {Uint8Array}
 */
export function encodeSkillsCuratorApplyRequest(snapshot_id, approved_actions_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(snapshot_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(approved_actions_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeSkillsCuratorApplyRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} snapshot_id
 * @returns {Uint8Array}
 */
export function encodeSkillsCuratorRollbackRequest(snapshot_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(snapshot_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSkillsCuratorRollbackRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeSkillsCuratorRunRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeSkillsCuratorRunRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} skill_id
 * @returns {Uint8Array}
 */
export function encodeSkillsDeleteRequest(skill_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(skill_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSkillsDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} skill_id
 * @returns {Uint8Array}
 */
export function encodeSkillsDetailRequest(skill_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(skill_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSkillsDetailRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} skill_id
 * @param {string} new_name
 * @returns {Uint8Array}
 */
export function encodeSkillsForkRequest(skill_id, new_name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(skill_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(new_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeSkillsForkRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} skill_id
 * @returns {Uint8Array}
 */
export function encodeSkillsHubApproveRequest(skill_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(skill_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSkillsHubApproveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} source
 * @param {string | null} [git_ref]
 * @returns {Uint8Array}
 */
export function encodeSkillsHubImportRequest(source, git_ref) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(source, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(git_ref) ? 0 : passStringToWasm0(git_ref, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeSkillsHubImportRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} skill_id
 * @returns {Uint8Array}
 */
export function encodeSkillsHubRejectRequest(skill_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(skill_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSkillsHubRejectRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} query
 * @param {string | null} [source]
 * @returns {Uint8Array}
 */
export function encodeSkillsHubSearchRequest(query, source) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(source) ? 0 : passStringToWasm0(source, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        wasm.encodeSkillsHubSearchRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string | null} [tag]
 * @param {string | null} [source]
 * @param {string | null} [status]
 * @returns {Uint8Array}
 */
export function encodeSkillsListRequest(tag, source, status) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(tag) ? 0 : passStringToWasm0(tag, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(source) ? 0 : passStringToWasm0(source, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(status) ? 0 : passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        wasm.encodeSkillsListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} skill_json
 * @returns {Uint8Array}
 */
export function encodeSkillsUpsertRequest(skill_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(skill_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSkillsUpsertRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::SsoProviderCreateRequest — pelne dane providera SSO/OIDC.
 * @param {string} name
 * @param {string} provider_type
 * @param {string} client_id
 * @param {string} client_secret
 * @param {string} discovery_url
 * @param {boolean} auto_create_users
 * @param {string | null} [default_group_id]
 * @returns {Uint8Array}
 */
export function encodeSsoProviderCreateRequest(name, provider_type, client_id, client_secret, discovery_url, auto_create_users, default_group_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(provider_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(client_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(client_secret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(discovery_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(default_group_id) ? 0 : passStringToWasm0(default_group_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        wasm.encodeSsoProviderCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, auto_create_users, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::SsoProviderDeleteRequest { id }.
 * @param {number} id
 * @returns {Uint8Array}
 */
export function encodeSsoProviderDeleteRequest(id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeSsoProviderDeleteRequest(retptr, id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::SsoProvidersListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeSsoProvidersListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeSsoProvidersListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} path
 * @returns {Uint8Array}
 */
export function encodeStorageBrowseRequest(path) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(path, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeStorageBrowseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} parent
 * @param {string} name
 * @returns {Uint8Array}
 */
export function encodeStorageCreateDirRequest(parent, name) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(parent, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeStorageCreateDirRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} key
 * @param {string} new_path
 * @param {boolean} move_data
 * @returns {Uint8Array}
 */
export function encodeStorageMigrateRequest(key, new_path, move_data) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(new_path, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeStorageMigrateRequest(retptr, ptr0, len0, ptr1, len1, move_data);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeStorageOverviewRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeStorageOverviewRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::StreamBody(CloseRequest) — release a live subscription early
 * (e.g. UI tile navigates away). Reuses the original correlation id; the
 * server cancels the streaming task and emits a final Closed frame.
 * @param {string} stream_id
 * @returns {Uint8Array}
 */
export function encodeStreamCloseRequest(stream_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(stream_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeStreamCloseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::StreamBody(SubscribeRequest) — subscribe this connection to a
 * hub-registered stream. The server first answers with a SubscribeResponse
 * (mime + has_init_segment), then pushes a sequence of Frame chunks on the
 * same correlation id, terminating with a single Closed payload. `preview`
 * wybiera wariant podglądu 720p/~1,5 Mbit/s dla strumieni `camera:` (kafelki
 * Live view); `false` = pełna jakość źródła.
 * @param {string} stream_id
 * @param {boolean} preview
 * @returns {Uint8Array}
 */
export function encodeStreamSubscribeRequest(stream_id, preview) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(stream_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeStreamSubscribeRequest(retptr, ptr0, len0, preview);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::SubscribeResumeRequest { resume_token }.
 * Klient po reconnect przekazuje token z poprzedniej SubscribeResumeOffer.
 * @param {Uint8Array} resume_token
 * @returns {Uint8Array}
 */
export function encodeSubscribeResumeRequest(resume_token) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(resume_token, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSubscribeResumeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} payload_json
 * @returns {Uint8Array}
 */
export function encodeSuggestServicePortRequest(payload_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(payload_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeSuggestServicePortRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} org_id
 * @param {string} addon_id
 * @param {string} operation_id
 * @param {string} resolution
 * @returns {Uint8Array}
 */
export function encodeSyncConflictResolveRequest(org_id, addon_id, operation_id, resolution) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(org_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(operation_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(resolution, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeSyncConflictResolveRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} org_id
 * @param {string} addon_id
 * @param {string} status
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeSyncConflictsListRequest(org_id, addon_id, status, limit) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(org_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeSyncConflictsListRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, limit);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeSyncStorageReportRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeSyncStorageReportRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(AccessLogRequest) — the "Dziennik dostępu" with its filters.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasAccessLogRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasAccessLogRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(AlertAckRequest) — acknowledge one alert.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasAlertAckRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasAlertAckRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(AlertForwardSetRequest) — where the node forwards its alerts
 * and (optionally) its access log; answers with AccessLogResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasAlertForwardSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasAlertForwardSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(AlertsListRequest) — open alerts, acknowledged ones too when `include_acked`.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasAlertsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasAlertsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ApprovalDecideRequest) — a second admin approves or rejects; answers with ApprovalsListResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasApprovalDecideRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasApprovalDecideRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ApprovalSettingsSetRequest) — the fleet-wide four-eyes switch.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasApprovalSettingsSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasApprovalSettingsSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ApprovalsListRequest) — the "waiting for approval" list plus the fleet switch.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasApprovalsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasApprovalsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ArcLimitSetRequest) — cap the ARC now and across reboots; answers with ArcStatsResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasArcLimitSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasArcLimitSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ArcStatsRequest) — the node's ARC counters and where its cap comes from.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasArcStatsRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasArcStatsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasConfigExportRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasConfigExportRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasConfigImportApplyRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasConfigImportApplyRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasConfigImportPlanRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasConfigImportPlanRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDatasetCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDatasetCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDatasetDestroyRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDatasetDestroyRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDatasetGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDatasetGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDatasetKeyRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDatasetKeyRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDatasetMountRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDatasetMountRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDatasetSetPropertiesRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDatasetSetPropertiesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDatasetsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDatasetsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(DiskGetRequest) — one disk with SMART attributes, self-tests, history and alerts.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDiskGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDiskGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(DiskLocateRequest) — blink the bay LED (`enable` toggles).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDiskLocateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDiskLocateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(DiskSmartTestRequest) — start a short/long SMART self-test; answers with JobResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDiskSmartTestRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDiskSmartTestRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(DisksListRequest) — inventory + live I/O + telemetry state.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasDisksListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasDisksListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Koduje żądanie ElasticArrayCreateRequest w rodzinie TentaNAS.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElasticArrayCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElasticArrayCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Koduje żądanie ElasticArrayGetRequest w rodzinie TentaNAS.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElasticArrayGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElasticArrayGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Koduje żądanie ElasticArrayPlanRequest w rodzinie TentaNAS.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElasticArrayPlanRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElasticArrayPlanRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Koduje żądanie ElasticArrayRestoreRequest w rodzinie TentaNAS.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElasticArrayRestoreRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElasticArrayRestoreRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Koduje żądanie ElasticArraysListRequest w rodzinie TentaNAS.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElasticArraysListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElasticArraysListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Koduje żądanie ElasticCapabilitiesRequest w rodzinie TentaNAS.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElasticCapabilitiesRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElasticCapabilitiesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ElevationArmRequest) — keep the sudo password in RAM for `ttl_secs` (0 = node default).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElevationArmRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElevationArmRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ElevationCatalogRequest) — everything the node's privilege helper may do.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElevationCatalogRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElevationCatalogRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ElevationDisarmRequest) — forget the armed password now.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElevationDisarmRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElevationDisarmRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ElevationPlanRequest) — what provisioning the helper would do, for the wizard's review step.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElevationPlanRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElevationPlanRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ElevationProvisionRequest) — install helper + sudoers; answers with JobResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElevationProvisionRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElevationProvisionRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(ElevationRemoveRequest) — remove helper + sudoers with a fresh password; answers with JobResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasElevationRemoveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasElevationRemoveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(EnvironmentRequest) — `refresh: true` re-probes instead of answering from cache.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasEnvironmentRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasEnvironmentRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasFleetMountRetryRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasFleetMountRetryRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasFleetMountsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasFleetMountsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(JobCancelRequest) — cooperative cancel of a running job.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasJobCancelRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasJobCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(JobGetRequest) — one job with its log.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasJobGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasJobGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(JobsListRequest) — recent jobs, newest first (`limit` 0 = default).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasJobsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasJobsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(NodesListRequest) — fleet list answered by the dashboard's own node.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasNodesListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasNodesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(PackagesInstallRequest) — install one feature's packages; answers with JobResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPackagesInstallRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPackagesInstallRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolAddVdevRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolAddVdevRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolDestroyRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolDestroyRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolDeviceStateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolDeviceStateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolExpandVdevRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolExpandVdevRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolExportRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolExportRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolImportRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolImportRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolImportScanRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolImportScanRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolPlanRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolPlanRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolRemoveVdevRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolRemoveVdevRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolReplaceDiskRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolReplaceDiskRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolScrubRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolScrubRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolSetPropertiesRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolSetPropertiesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(PoolTrimRequest) — 'start' | 'suspend' | 'resume' | 'cancel'.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolTrimRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolTrimRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasPoolsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasPoolsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSchedulesListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSchedulesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasScrubScheduleSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasScrubScheduleSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareBrowseRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareBrowseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareMountsRefreshRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareMountsRefreshRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareUpdateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareUserDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareUserDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareUserSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareUserSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasShareUsersListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasShareUsersListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSharesListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSharesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSmartScheduleSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSmartScheduleSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(SnapshotBrowseRequest) — list a directory inside a snapshot's `.zfs/snapshot` view.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotBrowseRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotBrowseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotCloneRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotCloneRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotDestroyRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotDestroyRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(SnapshotProtectionReleaseRequest) — always parks for a second admin; answers with ApprovalPendingResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotProtectionReleaseRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotProtectionReleaseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotRollbackRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotRollbackRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotScheduleDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotScheduleDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotScheduleSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotScheduleSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotSchedulesListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotSchedulesListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasSnapshotsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasSnapshotsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(TargetCreateRequest) — the n14 wizard. Answers with JobResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasTargetCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasTargetCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(TargetDeleteRequest) — retype-gated; the zvol and its data stay.
 * Answers with JobResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasTargetDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasTargetDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(TargetGetRequest) — one target with its sessions and the
 * rendered configfs it produces (secrets shown as `***`).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasTargetGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasTargetGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(TargetUpdateRequest) — portals, auth, the IQN/NQN allowlist and
 * the ALUA/ANA port groups. Answers with JobResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasTargetUpdateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasTargetUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(TargetsListRequest) — the block targets of the node (n12) plus
 * what this node can serve: LIO, nvmet, iSER, NVMe-oF/RDMA, DH-HMAC-CHAP, its interfaces
 * and the zvols the wizard may export.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasTargetsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasTargetsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaNasBody(TrimScheduleSetRequest) — the recurring trim of one pool.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaNasTrimScheduleSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaNasTrimScheduleSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(CircuitExportRequest) — canonical OpenQASM 3, a Qiskit program or the JSON IR.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantCircuitExportRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantCircuitExportRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(CircuitSimulateRequest) — starts a T1 run on the node that receives it.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantCircuitSimulateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantCircuitSimulateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(CircuitValidateRequest) — parse and check an OpenQASM 3 program without running it.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantCircuitValidateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantCircuitValidateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(FileDeleteRequest) — drops the project's reference; the content blob is the retention sweep's business.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantFileDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantFileDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(FileListRequest) — files of one project.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantFileListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantFileListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(FileUploadChunkRequest) — one 4 MiB chunk in
 * order (`bytes` is a Uint8Array on the JS side); the last chunk finalizes the
 * blob under its sha256 and answers with the stored `FileInfo`.
 * @param {string} instance_id
 * @param {string} project_id
 * @param {string} upload_id
 * @param {string} path
 * @param {string} kind
 * @param {number} seq
 * @param {number} total_chunks
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeTentaQuantFileUploadChunkRequest(instance_id, project_id, upload_id, path, kind, seq, total_chunks, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(instance_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(project_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(upload_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(path, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(kind, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ptr5 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len5 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantFileUploadChunkRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, seq, total_chunks, ptr5, len5);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v7 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v7;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(LabListRequest) — the laboratories the caller may enter.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantLabListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantLabListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(LabOverviewRequest) — dashboard counters of one laboratory.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantLabOverviewRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantLabOverviewRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(LabPeopleRequest) — matrix expansion; `quant.instruct` only.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantLabPeopleRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantLabPeopleRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(NotebookCreateRequest) — starts at version 1; `cells_json` is a JSON array.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantNotebookCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantNotebookCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(NotebookGetRequest) — head version, or the one named by `version`.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantNotebookGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantNotebookGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(NotebookListRequest) — notebooks of one project.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantNotebookListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantNotebookListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(NotebookSaveRequest) — optimistic locking: a stale `expected_version` answers Conflict.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantNotebookSaveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantNotebookSaveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(NotebookVersionsRequest) — the append-only version history.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantNotebookVersionsRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantNotebookVersionsRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(PeopleCandidatesRequest) — the organization's accounts for the share picker, each flagged with whether this laboratory admits them.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantPeopleCandidatesRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantPeopleCandidatesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectArchiveRequest) — archived projects are read-only.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectArchiveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectArchiveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectCreateRequest) — private by default; `visibility = "lab"` needs `quant.instruct`.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectCreateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectCreateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectDeleteRequest) — owner only; takes files, notebooks and versions with it.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectDeleteRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectGetRequest) — one project; the share list only for its owner.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectListRequest) — own ∪ shared ∪ published to the lab.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectShareRemoveRequest) — revoke one share.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectShareRemoveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectShareRemoveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectShareSetRequest) — grant `editor`/`viewer` on one project.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectShareSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectShareSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectTransferRequest) — hands the project to another member of the lab.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectTransferRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectTransferRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(ProjectUpdateRequest) — owner or editor, never a viewer.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantProjectUpdateRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantProjectUpdateRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunArtifactRequest) — a signed download URL for one stored output.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunArtifactRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunArtifactRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunCancelRequest) — asks a live run to stop between gates or shots.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunCancelRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunCompareRequest) — up to 8 runs on one aligned axis, with TVD and Hellinger fidelity against the first.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunCompareRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunCompareRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunExportRequest) — builds the scientific package of a run as one .zip and answers with its signed URL.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunExportRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunExportRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunGetRequest) — one run with its stored outputs.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunKeyframesRequest) — the recorded evolution of a finished run.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunKeyframesRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunKeyframesRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunListRequest) — runs the caller may see, newest first.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunPinRequest) — pins a run into the project's results gallery.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunPinRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunPinRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunStateQueryRequest) — reduced density matrices, mutual information and concurrence, computed on demand.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunStateQueryRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunStateQueryRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(RunSubscribeRequest) — the live run stream; `after_seq` resumes it.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantRunSubscribeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantRunSubscribeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(SettingsGetRequest) — the laboratory's settings document.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantSettingsGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantSettingsGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(SettingsSetRequest) — the operational document plus, for `quant.admin`, the admin half.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantSettingsSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantSettingsSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(TargetListRequest) — the tiers and nodes this laboratory offers.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantTargetListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantTargetListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaQuantBody(TargetResolveRequest) — the `device="auto"` rule, evaluated before a run starts.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaQuantTargetResolveRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaQuantTargetResolveRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(HostGetRequest) — one host with its last probe result.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmHostGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmHostGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(HostGrantsListRequest) — the H06 matrix for one host, with the subjects it may add.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmHostGrantsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmHostGrantsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(HostGrantsSetRequest) — the complete desired grant set of one host; answers with HostGrantsListResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmHostGrantsSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmHostGrantsSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(HostProbeRequest) — run the environment probe on the host that owns the hardware (`refresh: true` ignores the cache).
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmHostProbeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmHostProbeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(HostsListRequest) — every mesh node and connector host of the environment, filtered by the caller's `view` grants.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmHostsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmHostsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(InboxSnoozeRequest) — "Później" on one inbox item for `snooze_secs`; answers with SummaryResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmInboxSnoozeRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmInboxSnoozeRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(JobCancelRequest) — stop a job under its own `cancel_semantics`; answers with JobGetResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmJobCancelRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmJobCancelRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(JobGetRequest) — one job with its redacted log, forwarded to the job's owner node.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmJobGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmJobGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(JobsListRequest) — jobs of the environment, optionally narrowed to one host and a set of states.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmJobsListRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmJobsListRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(SettingsGetRequest) — the environment settings document.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmSettingsGetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmSettingsGetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(SettingsSetRequest) — write the whole settings document; answers with SettingsGetResponse.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmSettingsSetRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmSettingsSetRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TentaVmBody(SummaryRequest) — the P01 tiles plus the inbox, and the probe verdict P00 draws its empty state from.
 * @param {string} request_json
 * @returns {Uint8Array}
 */
export function encodeTentaVmSummaryRequest(request_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(request_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTentaVmSummaryRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TlsStatusRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeTlsStatusRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeTlsStatusRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeTokenCoordinatorStatusRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeTokenCoordinatorStatusRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} id
 * @returns {Uint8Array}
 */
export function encodeTokenDeleteQuotaRequest(id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTokenDeleteQuotaRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeTokenListQuotasRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeTokenListQuotasRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string | null | undefined} id
 * @param {string} scope_type
 * @param {string | null | undefined} subject_id
 * @param {string | null | undefined} model_id
 * @param {string} period
 * @param {bigint} max_total_tokens
 * @param {boolean} is_active
 * @returns {Uint8Array}
 */
export function encodeTokenUpsertQuotaRequest(id, scope_type, subject_id, model_id, period, max_total_tokens, is_active) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        var ptr0 = isLikeNone(id) ? 0 : passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(scope_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(subject_id) ? 0 : passStringToWasm0(subject_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(model_id) ? 0 : passStringToWasm0(model_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(period, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        wasm.encodeTokenUpsertQuotaRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, max_total_tokens, is_active);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v6 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v6;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} period
 * @param {string} period_key
 * @param {string} group_by
 * @returns {Uint8Array}
 */
export function encodeTokenUsageSummaryRequest(period, period_key, group_by) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(period, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(period_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(group_by, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeTokenUsageSummaryRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @returns {Uint8Array}
 */
export function encodeToolsCatalogRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeToolsCatalogRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TranslateRequest — synchroniczne tlumaczenie przez LLM.
 * `source_lang` = "auto" dla auto-detekcji; `tone` opcjonalny
 * ("formal"/"casual"/"neutral").
 * @param {string} source_text
 * @param {string} source_lang
 * @param {string} target_lang
 * @param {string | null} [tone]
 * @returns {Uint8Array}
 */
export function encodeTranslateRequest(source_text, source_lang, target_lang, tone) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(source_text, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(source_lang, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(target_lang, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(tone) ? 0 : passStringToWasm0(tone, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeTranslateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TtsPreviewRequest { text, model, voice } — podglad TTS
 * (synteza tekstu po czyszczeniu do audio, odtwarzane w panelu).
 * @param {string} text
 * @param {string} model
 * @param {string} voice
 * @returns {Uint8Array}
 */
export function encodeTtsPreviewRequest(text, model, voice) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(text, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(model, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(voice, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeTtsPreviewRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TtsRuleCreateRequest(TtsRule).
 * @param {string} id
 * @param {string} pattern
 * @param {string} voice_id
 * @param {number} priority
 * @returns {Uint8Array}
 */
export function encodeTtsRuleCreateRequest(id, pattern, voice_id, priority) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(pattern, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(voice_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        wasm.encodeTtsRuleCreateRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, priority);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v4 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v4;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TtsRuleDeleteRequest { rule_id }.
 * @param {string} rule_id
 * @returns {Uint8Array}
 */
export function encodeTtsRuleDeleteRequest(rule_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(rule_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeTtsRuleDeleteRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::TtsRuleListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeTtsRuleListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeTtsRuleListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Encode Action into MessageBody::UiChannelCbor frame.
 * @param {string} addon_id
 * @param {string} panel_id
 * @param {bigint} panel_epoch
 * @param {string} action_id
 * @param {string} params_json
 * @returns {Uint8Array}
 */
export function encodeUiAction(addon_id, panel_id, panel_epoch, action_id, params_json) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(panel_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(action_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(params_json, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeUiAction(retptr, ptr0, len0, ptr1, len1, panel_epoch, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Wraps raw CBOR bytes in `MessageBody::UiChannelCbor` for binary WS transport.
 * @param {Uint8Array} cbor_bytes
 * @returns {Uint8Array}
 */
export function encodeUiChannelCbor(cbor_bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(cbor_bytes, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeUiChannelCbor(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Encode PanelClose into MessageBody::UiChannelCbor frame.
 * @param {string} addon_id
 * @param {string} panel_id
 * @param {bigint} panel_epoch
 * @returns {Uint8Array}
 */
export function encodeUiPanelClose(addon_id, panel_id, panel_epoch) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(panel_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeUiPanelClose(retptr, ptr0, len0, ptr1, len1, panel_epoch);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Encode PanelOpen into MessageBody::UiChannelCbor frame.
 * @param {string} addon_id
 * @param {string} panel_id
 * @param {string} locale
 * @param {string} theme
 * @param {number} viewport_width
 * @param {number} viewport_height
 * @returns {Uint8Array}
 */
export function encodeUiPanelOpen(addon_id, panel_id, locale, theme, viewport_width, viewport_height) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(panel_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(locale, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(theme, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        wasm.encodeUiPanelOpen(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, viewport_width, viewport_height);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * LEGACY UsersListRequest — zastapione przez encodeIamListUsersRequest.
 * @returns {Uint8Array}
 */
export function encodeUsersListRequest() {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeUsersListRequest(retptr);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} manifest_url
 * @param {string} api_key
 * @returns {Uint8Array}
 */
export function encodeVisionImportFetchManifestRequest(manifest_url, api_key) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(manifest_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(api_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeVisionImportFetchManifestRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * @param {string} manifest_url
 * @param {string} api_key
 * @param {string} model_name
 * @param {string | null} [alias]
 * @returns {Uint8Array}
 */
export function encodeVisionImportModelRequest(manifest_url, api_key, model_name, alias) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(manifest_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(api_key, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(model_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(alias) ? 0 : passStringToWasm0(alias, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        wasm.encodeVisionImportModelRequest(retptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v5 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v5;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::VisionBody(InferRequest) — encoder Vision inference.
 * @param {string} service_name
 * @param {Uint8Array} image
 * @param {number | null} [width]
 * @param {number | null} [height]
 * @returns {Uint8Array}
 */
export function encodeVisionInferRequest(service_name, image, width, height) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(service_name, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(image, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeVisionInferRequest(retptr, ptr0, len0, ptr1, len1, isLikeNone(width) ? Number.MAX_SAFE_INTEGER : (width) >>> 0, isLikeNone(height) ? Number.MAX_SAFE_INTEGER : (height) >>> 0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::VncTunnelBody(ReqClose) — tear down tunnel explicitly.
 * @param {string} tunnel_id
 * @returns {Uint8Array}
 */
export function encodeVncTunnelCloseRequest(tunnel_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(tunnel_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.encodeVncTunnelCloseRequest(retptr, ptr0, len0);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v2 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v2;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::VncTunnelBody(ReqOpen) — start streaming tunnel for session.
 * @param {number} session_id
 * @returns {Uint8Array}
 */
export function encodeVncTunnelOpenRequest(session_id) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.encodeVncTunnelOpenRequest(retptr, session_id);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v1 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v1;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * MessageBody::VncTunnelBody(ReqSend) — browser → container RFB bytes.
 * @param {string} tunnel_id
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeVncTunnelSendRequest(tunnel_id, bytes) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(tunnel_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        wasm.encodeVncTunnelSendRequest(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Przepina gotowy frame na Routing::Forward do wskazanego wezla (64-znakowy
 * hex Ed25519 node id). Jedna funkcja zamiast wariantu Forward w kazdym
 * encoderze: klient buduje frame jak zwykle i opcjonalnie go adresuje.
 * Serwer (wezel dashboardu) mintuje asercje i przekazuje body przez mesh —
 * klient nie niesie zadnego materialu kryptograficznego.
 * @param {Uint8Array} frame
 * @param {string} target_node_hex
 * @returns {Uint8Array}
 */
export function envelopeSetForward(frame, target_node_hex) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passArray8ToWasm0(frame, wasm.__wbindgen_export);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_node_hex, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        wasm.envelopeSetForward(retptr, ptr0, len0, ptr1, len1);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        var r2 = getDataViewMemory0().getInt32(retptr + 4 * 2, true);
        var r3 = getDataViewMemory0().getInt32(retptr + 4 * 3, true);
        if (r3) {
            throw takeObject(r2);
        }
        var v3 = getArrayU8FromWasm0(r0, r1).slice();
        wasm.__wbindgen_export3(r0, r1 * 1, 1);
        return v3;
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

/**
 * Stale discriminantow message_kind dla dispatchu po stronie JS.
 * Wolac `messageKind()` raz, cachowac result.
 * @returns {any}
 */
export function messageKind() {
    const ret = wasm.messageKind();
    return takeObject(ret);
}

/**
 * Szybka walidacja ze bajty maja prawidlowy ksztalt (pelny bytecheck envelope)
 * bez zwracania widoku. Uzyte do wczesnego odrzucenia malformed frames przed
 * enqueue do dispatch queue.
 * @param {Uint8Array} bytes
 * @returns {boolean}
 */
export function validateFrame(bytes) {
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_export);
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
        __wbg_Error_fdd633d4bb5dd76a: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg___wbindgen_debug_string_8a447059637473e2: function(arg0, arg1) {
            const ret = debugString(getObject(arg1));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_null_6d937fbfb6478470: function(arg0) {
            const ret = getObject(arg0) === null;
            return ret;
        },
        __wbg___wbindgen_is_object_0beba4a1980d3eea: function(arg0) {
            const val = getObject(arg0);
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_undefined_721f8decd50c87a3: function(arg0) {
            const ret = getObject(arg0) === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_1cc01dd708740256: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_71bb4348194e31f0: function(arg0, arg1) {
            const obj = getObject(arg1);
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_ea4887a5f8f9a9db: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_error_a6fa202b58aa1cd3: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_export3(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_from_50138b2ca136f50c: function(arg0) {
            const ret = Array.from(getObject(arg0));
            return addHeapObject(ret);
        },
        __wbg_getItem_f2b45bf1b0166c48: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = getObject(arg1).getItem(getStringFromWasm0(arg2, arg3));
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_getRandomValues_cc7f052a444bb2ce: function() { return handleError(function (arg0, arg1) {
            globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_get_197a3fe98f169e38: function(arg0, arg1) {
            const ret = getObject(arg0)[arg1 >>> 0];
            return addHeapObject(ret);
        },
        __wbg_get_dddb90ff5d27a080: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(getObject(arg0), getObject(arg1));
            return addHeapObject(ret);
        }, arguments); },
        __wbg_instanceof_Window_0d356b88a2f77c42: function(arg0) {
            let result;
            try {
                result = getObject(arg0) instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_isArray_145a34fd0a38d37b: function(arg0) {
            const ret = Array.isArray(getObject(arg0));
            return ret;
        },
        __wbg_isArray_ffd528e87c3c8cef: function(arg0) {
            const ret = Array.isArray(getObject(arg0));
            return ret;
        },
        __wbg_length_589238bdcf171f0e: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_length_c6054974c0a6cdb9: function(arg0) {
            const ret = getObject(arg0).length;
            return ret;
        },
        __wbg_localStorage_8daa25c913870d2f: function() { return handleError(function (arg0) {
            const ret = getObject(arg0).localStorage;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        }, arguments); },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return addHeapObject(ret);
        },
        __wbg_new_2e117a478906f062: function() {
            const ret = new Object();
            return addHeapObject(ret);
        },
        __wbg_new_36e147a8ced3c6e0: function() {
            const ret = new Array();
            return addHeapObject(ret);
        },
        __wbg_new_from_slice_543b875b27789a8f: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_from_slice_6696ab0c133d3f19: function(arg0, arg1) {
            const ret = new Float32Array(getArrayF32FromWasm0(arg0, arg1));
            return addHeapObject(ret);
        },
        __wbg_new_with_length_9b650f44b5c44a4e: function(arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return addHeapObject(ret);
        },
        __wbg_push_f724b5db8acf89d2: function(arg0, arg1) {
            const ret = getObject(arg0).push(getObject(arg1));
            return ret;
        },
        __wbg_removeItem_dbf2a2abce661547: function() { return handleError(function (arg0, arg1, arg2) {
            getObject(arg0).removeItem(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_setItem_ab73a1e4497df37e: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            getObject(arg0).setItem(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_set_0bf1fca872bc6d18: function(arg0, arg1, arg2) {
            getObject(arg0).set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_set_4564f7dc44fcb0c9: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(getObject(arg0), getObject(arg1), getObject(arg2));
            return ret;
        }, arguments); },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = getObject(arg1).stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_static_accessor_GLOBAL_THIS_2fee5048bcca5938: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_GLOBAL_ce44e66a4935da8c: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_SELF_44f6e0cb5e67cdad: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbg_static_accessor_WINDOW_168f178805d978fe: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000001: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000002: function(arg0) {
            // Cast intrinsic for `I64 -> Externref`.
            const ret = arg0;
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return addHeapObject(ret);
        },
        __wbindgen_cast_0000000000000004: function(arg0) {
            // Cast intrinsic for `U64 -> Externref`.
            const ret = BigInt.asUintN(64, arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_clone_ref: function(arg0) {
            const ret = getObject(arg0);
            return addHeapObject(ret);
        },
        __wbindgen_object_drop_ref: function(arg0) {
            takeObject(arg0);
        },
    };
    return {
        __proto__: null,
        "./wasm_glue_bg.js": import0,
    };
}

const EnvelopeViewFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_envelopeview_free(ptr, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayF32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getFloat32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
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

let cachedFloat32ArrayMemory0 = null;
function getFloat32ArrayMemory0() {
    if (cachedFloat32ArrayMemory0 === null || cachedFloat32ArrayMemory0.byteLength === 0) {
        cachedFloat32ArrayMemory0 = new Float32Array(wasm.memory.buffer);
    }
    return cachedFloat32ArrayMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        wasm.__wbindgen_export4(addHeapObject(e));
    }
}

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

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
    const mem = getDataViewMemory0();
    for (let i = 0; i < array.length; i++) {
        mem.setUint32(ptr + 4 * i, addHeapObject(array[i]), true);
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

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
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

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedFloat32ArrayMemory0 = null;
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
