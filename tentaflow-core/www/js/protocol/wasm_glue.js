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
 * Zwraca hex Ed25519 public key (64 znaki). Generuje keypair przy pierwszym
 * uzyciu i persistuje w localStorage.
 * @returns {string}
 */
export function browserNodeId() {
    let deferred2_0;
    let deferred2_1;
    try {
        const ret = wasm.browserNodeId();
        var ptr1 = ret[0];
        var len1 = ret[1];
        if (ret[3]) {
            ptr1 = 0; len1 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred2_0 = ptr1;
        deferred2_1 = len1;
        return getStringFromWasm0(ptr1, len1);
    } finally {
        wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
    }
}

/**
 * Usuwa keypair z localStorage (wylogowanie/reset tozsamosci browser).
 * Kolejne wywolanie `browserNodeId` wygeneruje nowy keypair.
 */
export function browserResetIdentity() {
    const ret = wasm.browserResetIdentity();
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

/**
 * Podpisuje `data` i zwraca raw bajty podpisu (64 B).
 * @param {Uint8Array} data
 * @returns {Uint8Array}
 */
export function browserSign(data) {
    const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.browserSign(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.browserSignHex(ptr0, len0);
        var ptr2 = ret[0];
        var len2 = ret[1];
        if (ret[3]) {
            ptr2 = 0; len2 = 0;
            throw takeFromExternrefTable0(ret[2]);
        }
        deferred3_0 = ptr2;
        deferred3_1 = len2;
        return getStringFromWasm0(ptr2, len2);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
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
    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.decodeEnvelope(ptr0, len0);
    if (ret[2]) {
        throw takeFromExternrefTable0(ret[1]);
    }
    return EnvelopeView.__wrap(ret[0]);
}

/**
 * Dekoduje rkyv-zakodowany MessageBody na JS object.
 * Dla znanych variantow zwraca obiekt z polem `variant`, a dla nieznanego
 * variantu `{ variant: "Unknown" }`.
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
 * MessageBody::AddonAdminOnlySetRequest { addon_id, admin_only }.
 * @param {string} addon_id
 * @param {boolean} admin_only
 * @returns {Uint8Array}
 */
export function encodeAddonAdminOnlySetRequest(addon_id, admin_only) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonAdminOnlySetRequest(ptr0, len0, admin_only);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonConfigGetRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonConfigGetRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArrayJsValueToWasm0(keys, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArrayJsValueToWasm0(values, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonConfigSetRequest(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * MessageBody::AddonDetailRequest { addon_id } — szczegoly addona.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonDetailRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonDetailRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} filename
 * @param {Uint8Array} content
 * @returns {Uint8Array}
 */
export function encodeAddonInstallRequest(filename, content) {
    const ptr0 = passStringToWasm0(filename, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(content, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonInstallRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
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
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(level) ? 0 : passStringToWasm0(level, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(search) ? 0 : passStringToWasm0(search, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonLogsRequest(ptr0, len0, limit, offset, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonNetworkRulesGetRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonNetworkRulesGetRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} addon_id
 * @param {string[]} allowed_hosts
 * @param {string[]} blocked_hosts
 * @param {string} mode
 * @returns {Uint8Array}
 */
export function encodeAddonNetworkRulesSetRequest(addon_id, allowed_hosts, blocked_hosts, mode) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArrayJsValueToWasm0(allowed_hosts, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArrayJsValueToWasm0(blocked_hosts, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(mode, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonNetworkRulesSetRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
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
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(mode, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(redirect_after) ? 0 : passStringToWasm0(redirect_after, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonOAuthAuthorizeStartRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
}

/**
 * MessageBody::AddonOAuthConfigClearSecretRequest — usun wylacznie secret.
 * @param {string} addon_id
 * @param {string} provider_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthConfigClearSecretRequest(addon_id, provider_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonOAuthConfigClearSecretRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::AddonOAuthConfigListRequest { addon_id } — zero secretow.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthConfigListRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonOAuthConfigListRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(client_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(client_secret) ? 0 : passStringToWasm0(client_secret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    const ptr4 = passStringToWasm0(redirect_uri, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len4 = WASM_VECTOR_LEN;
    const ptr5 = passStringToWasm0(oauth_mode, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len5 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonOAuthConfigSetRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, enabled, ptr5, len5);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v7 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v7;
}

/**
 * MessageBody::AddonOAuthLinkedAccountsRequest — lista polaczonych kont.
 * `scope` = "all" (admin) lub "mine" (user).
 * @param {string} addon_id
 * @param {string} scope
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthLinkedAccountsRequest(addon_id, scope) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(scope, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonOAuthLinkedAccountsRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::AddonOAuthReauthorizeRequest { account_id }.
 * @param {number} account_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthReauthorizeRequest(account_id) {
    const ret = wasm.encodeAddonOAuthReauthorizeRequest(account_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::AddonOAuthRevokeRequest { account_id }.
 * @param {number} account_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthRevokeRequest(account_id) {
    const ret = wasm.encodeAddonOAuthRevokeRequest(account_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::AddonOAuthTestConnectionRequest { addon_id, provider_id }.
 * @param {string} addon_id
 * @param {string} provider_id
 * @returns {Uint8Array}
 */
export function encodeAddonOAuthTestConnectionRequest(addon_id, provider_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(provider_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonOAuthTestConnectionRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::AddonPermissionCatalogRequest { addon_id } — katalog deklaracji.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionCatalogRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonPermissionCatalogRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::AddonPermissionCheckRequest — czy uzytkownik ma uprawnienie.
 * `user_id` = None (pass null z JS) => serwer uzyje id z sesji.
 * @param {string} addon_id
 * @param {string} permission_id
 * @param {number | null} [user_id]
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionCheckRequest(addon_id, permission_id, user_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(permission_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonPermissionCheckRequest(ptr0, len0, ptr1, len1, !isLikeNone(user_id), isLikeNone(user_id) ? 0 : user_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::AddonPermissionDefaultSetRequest — ustawia domyslny grant addona.
 * @param {string} addon_id
 * @param {string} permission_id
 * @param {string} grant_mode
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionDefaultSetRequest(addon_id, permission_id, grant_mode) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(permission_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(grant_mode, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonPermissionDefaultSetRequest(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * MessageBody::AddonPermissionMatrixRequest { addon_id } — aktualna macierz.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionMatrixRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonPermissionMatrixRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::AddonPermissionSetRequest — ustawia grant per (user|group).
 * @param {string} addon_id
 * @param {string} subject_type
 * @param {number} subject_id
 * @param {string} permission_id
 * @param {string} grant_mode
 * @returns {Uint8Array}
 */
export function encodeAddonPermissionSetRequest(addon_id, subject_type, subject_id, permission_id, grant_mode) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(subject_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(permission_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(grant_mode, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonPermissionSetRequest(ptr0, len0, ptr1, len1, subject_id, ptr2, len2, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonReloadRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonReloadRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonResourcesGetRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonResourcesGetRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonResourcesSetRequest(ptr0, len0, max_instances, cpu_limit_pct, ram_mb, storage_mb, http_requests_per_min, llm_tokens_per_min);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::AddonShowInCatalogSetRequest { addon_id, show_in_catalog }.
 * @param {string} addon_id
 * @param {boolean} show_in_catalog
 * @returns {Uint8Array}
 */
export function encodeAddonShowInCatalogSetRequest(addon_id, show_in_catalog) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonShowInCatalogSetRequest(ptr0, len0, show_in_catalog);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} addon_id
 * @param {boolean} enabled
 * @returns {Uint8Array}
 */
export function encodeAddonToggleRequest(addon_id, enabled) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonToggleRequest(ptr0, len0, enabled);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonToolsRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonToolsRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonUninstallRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonUninstallRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::AddonVisibilityListRequest { addon_id } — widocznosc per grupa.
 * @param {string} addon_id
 * @returns {Uint8Array}
 */
export function encodeAddonVisibilityListRequest(addon_id) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonVisibilityListRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::AddonVisibilitySetRequest { addon_id, group_id, visible }.
 * @param {string} addon_id
 * @param {number} group_id
 * @param {boolean} visible
 * @returns {Uint8Array}
 */
export function encodeAddonVisibilitySetRequest(addon_id, group_id, visible) {
    const ptr0 = passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAddonVisibilitySetRequest(ptr0, len0, group_id, visible);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::AddonsListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeAddonsListRequest() {
    const ret = wasm.encodeAddonsListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
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
 * MessageBody::AuditLogCleanupRequest — usun wpisy starsze niz N dni.
 * @param {number} keep_days
 * @returns {Uint8Array}
 */
export function encodeAuditLogCleanupRequest(keep_days) {
    const ret = wasm.encodeAuditLogCleanupRequest(keep_days);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::AuditLogExportRequest — eksport CSV z filtrami.
 * @param {number | null} [user_id]
 * @param {string | null} [addon_id]
 * @param {string | null} [action]
 * @param {string | null} [from_date]
 * @param {string | null} [to_date]
 * @param {string | null} [search]
 * @returns {Uint8Array}
 */
export function encodeAuditLogExportRequest(user_id, addon_id, action, from_date, to_date, search) {
    var ptr0 = isLikeNone(addon_id) ? 0 : passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(action) ? 0 : passStringToWasm0(action, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(from_date) ? 0 : passStringToWasm0(from_date, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(to_date) ? 0 : passStringToWasm0(to_date, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    var ptr4 = isLikeNone(search) ? 0 : passStringToWasm0(search, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len4 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAuditLogExportRequest(!isLikeNone(user_id), isLikeNone(user_id) ? 0 : user_id, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v6 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v6;
}

/**
 * MessageBody::AuditLogListRequest — lista logu z filtrami + paginacja.
 * @param {number | null | undefined} user_id
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
    var ptr0 = isLikeNone(addon_id) ? 0 : passStringToWasm0(addon_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(action) ? 0 : passStringToWasm0(action, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(from_date) ? 0 : passStringToWasm0(from_date, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(to_date) ? 0 : passStringToWasm0(to_date, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    var ptr4 = isLikeNone(search) ? 0 : passStringToWasm0(search, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len4 = WASM_VECTOR_LEN;
    const ret = wasm.encodeAuditLogListRequest(!isLikeNone(user_id), isLikeNone(user_id) ? 0 : user_id, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, offset, limit);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v6 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v6;
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
 * MessageBody::BrowserCaptureRequest — one-shot capture of the bot's page.
 * @param {number} session_id
 * @param {string} kind
 * @param {boolean} full_page
 * @returns {Uint8Array}
 */
export function encodeBrowserCaptureRequest(session_id, kind, full_page) {
    const ptr0 = passStringToWasm0(kind, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeBrowserCaptureRequest(session_id, ptr0, len0, full_page);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
 * MessageBody::ClusterAddMemberRequest.
 * @param {string} cluster_id
 * @param {string} node_id
 * @param {string | null} [interface_type]
 * @param {number | null} [interface_speed_mbps]
 * @returns {Uint8Array}
 */
export function encodeClusterAddMemberRequest(cluster_id, node_id, interface_type, interface_speed_mbps) {
    const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(interface_type) ? 0 : passStringToWasm0(interface_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterAddMemberRequest(ptr0, len0, ptr1, len1, ptr2, len2, isLikeNone(interface_speed_mbps) ? 0x100000001 : (interface_speed_mbps) >>> 0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
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
    const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(strategy, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(failover_target) ? 0 : passStringToWasm0(failover_target, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterCreateRequest(ptr0, len0, ptr1, len1, ptr2, len2, failover_enabled, ptr3, len3, health_check_interval_ms, timeout_ms);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
}

/**
 * MessageBody::ClusterDeleteRequest { cluster_id }.
 * @param {string} cluster_id
 * @returns {Uint8Array}
 */
export function encodeClusterDeleteRequest(cluster_id) {
    const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterDeleteRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ClusterDetailRequest { cluster_id }.
 * @param {string} cluster_id
 * @returns {Uint8Array}
 */
export function encodeClusterDetailRequest(cluster_id) {
    const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterDetailRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ClusterListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeClusterListRequest() {
    const ret = wasm.encodeClusterListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::ClusterProbeStreamRequest { node_ids }.
 * @param {string[]} node_ids
 * @returns {Uint8Array}
 */
export function encodeClusterProbeStreamRequest(node_ids) {
    const ptr0 = passArrayJsValueToWasm0(node_ids, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterProbeStreamRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ClusterRemoveMemberRequest.
 * @param {string} cluster_id
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeClusterRemoveMemberRequest(cluster_id, node_id) {
    const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterRemoveMemberRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
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
    const ptr0 = passStringToWasm0(cluster_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(name) ? 0 : passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(strategy) ? 0 : passStringToWasm0(strategy, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    var ptr4 = isLikeNone(failover_target) ? 0 : passStringToWasm0(failover_target, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len4 = WASM_VECTOR_LEN;
    const ret = wasm.encodeClusterUpdateRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, isLikeNone(failover_enabled) ? 0xFFFFFF : failover_enabled ? 1 : 0, ptr4, len4, isLikeNone(health_check_interval_ms) ? 0x100000001 : (health_check_interval_ms) >>> 0, isLikeNone(timeout_ms) ? 0x100000001 : (timeout_ms) >>> 0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v6 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v6;
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
 * @param {string} engine_id
 * @param {string} status
 * @param {boolean} only_mine
 * @param {number} limit
 * @returns {Uint8Array}
 */
export function encodeDeploymentListRequest(engine_id, status, only_mine, limit) {
    const ptr0 = passStringToWasm0(engine_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(status, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeDeploymentListRequest(ptr0, len0, ptr1, len1, only_mine, limit);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * @param {string} deploy_id
 * @param {boolean} replay_tail
 * @returns {Uint8Array}
 */
export function encodeDeploymentLogStreamRequest(deploy_id, replay_tail) {
    const ptr0 = passStringToWasm0(deploy_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeDeploymentLogStreamRequest(ptr0, len0, replay_tail);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} deploy_id
 * @returns {Uint8Array}
 */
export function encodeDeploymentStatusRequest(deploy_id) {
    const ptr0 = passStringToWasm0(deploy_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeDeploymentStatusRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
 * MessageBody::FastPathListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeFastPathListRequest() {
    const ret = wasm.encodeFastPathListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::FlowCreateRequest { name, description, graph_json }.
 * @param {string} name
 * @param {string | null | undefined} description
 * @param {string} graph_json
 * @returns {Uint8Array}
 */
export function encodeFlowCreateRequest(name, description, graph_json) {
    const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(graph_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowCreateRequest(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * MessageBody::FlowDeleteRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowDeleteRequest(flow_id) {
    const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowDeleteRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::FlowDetailRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowDetailRequest(flow_id) {
    const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowDetailRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::FlowExecutionsListRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowExecutionsListRequest(flow_id) {
    const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowExecutionsListRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::FlowListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeFlowListRequest() {
    const ret = wasm.encodeFlowListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::FlowNodeTemplatesListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeFlowNodeTemplatesListRequest() {
    const ret = wasm.encodeFlowNodeTemplatesListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::FlowUpdateRequest — partial update flow.
 * @param {string} flow_id
 * @param {string | null} [name]
 * @param {string | null} [description]
 * @param {string | null} [flow_json]
 * @param {string | null} [status]
 * @returns {Uint8Array}
 */
export function encodeFlowUpdateRequest(flow_id, name, description, flow_json, status) {
    const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(name) ? 0 : passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(description) ? 0 : passStringToWasm0(description, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(flow_json) ? 0 : passStringToWasm0(flow_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    var ptr4 = isLikeNone(status) ? 0 : passStringToWasm0(status, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len4 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowUpdateRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v6 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v6;
}

/**
 * MessageBody::FlowVersionGetRequest { flow_id, version_id }.
 * @param {string} flow_id
 * @param {string} version_id
 * @returns {Uint8Array}
 */
export function encodeFlowVersionGetRequest(flow_id, version_id) {
    const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(version_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowVersionGetRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::FlowVersionListRequest { flow_id }.
 * @param {string} flow_id
 * @returns {Uint8Array}
 */
export function encodeFlowVersionListRequest(flow_id) {
    const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowVersionListRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::FlowVersionRestoreRequest { flow_id, version_id }.
 * @param {string} flow_id
 * @param {string} version_id
 * @returns {Uint8Array}
 */
export function encodeFlowVersionRestoreRequest(flow_id, version_id) {
    const ptr0 = passStringToWasm0(flow_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(version_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeFlowVersionRestoreRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::HubEngineListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeHubEngineListRequest() {
    const ret = wasm.encodeHubEngineListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::HubModelSearchRequest { query }.
 * @param {string} query
 * @returns {Uint8Array}
 */
export function encodeHubModelSearchRequest(query) {
    const ptr0 = passStringToWasm0(query, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeHubModelSearchRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} resource_type
 * @param {string} resource_id
 * @param {string} subject_type
 * @param {number} subject_id
 * @returns {Uint8Array}
 */
export function encodeIamClearPermissionRequest(resource_type, resource_id, subject_type, subject_id) {
    const ptr0 = passStringToWasm0(resource_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(resource_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(subject_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamClearPermissionRequest(ptr0, len0, ptr1, len1, ptr2, len2, subject_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * @param {string} name
 * @param {string} description
 * @returns {Uint8Array}
 */
export function encodeIamCreateGroupRequest(name, description) {
    const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(description, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamCreateGroupRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
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
    const ptr0 = passStringToWasm0(username, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(display_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(email, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ptr4 = passStringToWasm0(role, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len4 = WASM_VECTOR_LEN;
    const ptr5 = passStringToWasm0(group_ids_csv, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len5 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamCreateUserRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v7 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v7;
}

/**
 * @param {number} group_id
 * @returns {Uint8Array}
 */
export function encodeIamDeleteGroupRequest(group_id) {
    const ret = wasm.encodeIamDeleteGroupRequest(group_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {number} user_id
 * @returns {Uint8Array}
 */
export function encodeIamDeleteUserRequest(user_id) {
    const ret = wasm.encodeIamDeleteUserRequest(user_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {number} user_id
 * @returns {Uint8Array}
 */
export function encodeIamGetUserRequest(user_id) {
    const ret = wasm.encodeIamGetUserRequest(user_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {number} group_id
 * @returns {Uint8Array}
 */
export function encodeIamGroupMembersRequest(group_id) {
    const ret = wasm.encodeIamGroupMembersRequest(group_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @returns {Uint8Array}
 */
export function encodeIamListGroupsRequest() {
    const ret = wasm.encodeIamListGroupsRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {string} resource_type
 * @param {string} resource_id
 * @returns {Uint8Array}
 */
export function encodeIamListPermsForResourceRequest(resource_type, resource_id) {
    const ptr0 = passStringToWasm0(resource_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(resource_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamListPermsForResourceRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * @param {string} subject_type
 * @param {number} subject_id
 * @returns {Uint8Array}
 */
export function encodeIamListPermsForSubjectRequest(subject_type, subject_id) {
    const ptr0 = passStringToWasm0(subject_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamListPermsForSubjectRequest(ptr0, len0, subject_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @returns {Uint8Array}
 */
export function encodeIamListUsersRequest() {
    const ret = wasm.encodeIamListUsersRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {number} user_id
 * @param {string} new_password
 * @returns {Uint8Array}
 */
export function encodeIamResetUserPasswordRequest(user_id, new_password) {
    const ptr0 = passStringToWasm0(new_password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamResetUserPasswordRequest(user_id, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} resource_type
 * @param {string} resource_id
 * @param {string} subject_type
 * @param {number} subject_id
 * @param {string} access_level
 * @returns {Uint8Array}
 */
export function encodeIamSetPermissionRequest(resource_type, resource_id, subject_type, subject_id, access_level) {
    const ptr0 = passStringToWasm0(resource_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(resource_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(subject_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(access_level, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamSetPermissionRequest(ptr0, len0, ptr1, len1, ptr2, len2, subject_id, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
}

/**
 * @param {number} user_id
 * @param {string} group_ids_csv
 * @returns {Uint8Array}
 */
export function encodeIamSetUserGroupsRequest(user_id, group_ids_csv) {
    const ptr0 = passStringToWasm0(group_ids_csv, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamSetUserGroupsRequest(user_id, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {number} group_id
 * @param {string} name
 * @param {string} description
 * @returns {Uint8Array}
 */
export function encodeIamUpdateGroupRequest(group_id, name, description) {
    const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(description, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamUpdateGroupRequest(group_id, ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * @param {number} user_id
 * @param {string} display_name
 * @param {string} email
 * @param {boolean} is_active
 * @param {string} role
 * @returns {Uint8Array}
 */
export function encodeIamUpdateUserRequest(user_id, display_name, email, is_active, role) {
    const ptr0 = passStringToWasm0(display_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(email, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(role, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeIamUpdateUserRequest(user_id, ptr0, len0, ptr1, len1, is_active, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * @param {number} item_id
 * @param {string} status
 * @returns {Uint8Array}
 */
export function encodeMeetingActionItemStatusUpdateRequest(item_id, status) {
    const ptr0 = passStringToWasm0(status, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeetingActionItemStatusUpdateRequest(item_id, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} meeting_key
 * @param {string | null} [status_filter]
 * @returns {Uint8Array}
 */
export function encodeMeetingActionItemsListRequest(meeting_key, status_filter) {
    const ptr0 = passStringToWasm0(meeting_key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(status_filter) ? 0 : passStringToWasm0(status_filter, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeetingActionItemsListRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeetingActiveSessionRequest() {
    const ret = wasm.encodeMeetingActiveSessionRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {number} session_id
 * @param {boolean} include_transcripts
 * @returns {Uint8Array}
 */
export function encodeMeetingSessionDetailRequest(session_id, include_transcripts) {
    const ret = wasm.encodeMeetingSessionDetailRequest(session_id, include_transcripts);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {number} session_id
 * @returns {Uint8Array}
 */
export function encodeMeetingSessionLeaveRequest(session_id) {
    const ret = wasm.encodeMeetingSessionLeaveRequest(session_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {boolean} only_mine
 * @returns {Uint8Array}
 */
export function encodeMeetingSessionListRequest(only_mine) {
    const ret = wasm.encodeMeetingSessionListRequest(only_mine);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
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
    const ptr0 = passStringToWasm0(meeting_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(title, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(platform, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(bot_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ptr4 = passStringToWasm0(stt_alias, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len4 = WASM_VECTOR_LEN;
    const ptr5 = passStringToWasm0(tts_alias, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len5 = WASM_VECTOR_LEN;
    const ptr6 = passStringToWasm0(llm_alias, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len6 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeetingSessionStartRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5, ptr6, len6);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v8 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v8;
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeetingSettingsGetRequest() {
    const ret = wasm.encodeMeetingSettingsGetRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * `settings` jest JS Array<[key, value]>. Konwertujemy pary do Vec<MeetingSettingKv>.
 * @param {any} settings
 * @returns {Uint8Array}
 */
export function encodeMeetingSettingsUpdateRequest(settings) {
    const ret = wasm.encodeMeetingSettingsUpdateRequest(settings);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {string} meeting_key
 * @param {number | null} [limit]
 * @returns {Uint8Array}
 */
export function encodeMeetingSummariesListRequest(meeting_key, limit) {
    const ptr0 = passStringToWasm0(meeting_key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeetingSummariesListRequest(ptr0, len0, isLikeNone(limit) ? 0x100000001 : (limit) >>> 0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} meeting_key
 * @returns {Uint8Array}
 */
export function encodeMeetingTranscriptExportRequest(meeting_key) {
    const ptr0 = passStringToWasm0(meeting_key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeetingTranscriptExportRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {number} session_id
 * @param {number} since_ms
 * @returns {Uint8Array}
 */
export function encodeMeetingTranscriptsListRequest(session_id, since_ms) {
    const ret = wasm.encodeMeetingTranscriptsListRequest(session_id, since_ms);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {string} address
 * @returns {Uint8Array}
 */
export function encodeMeshConnectRequest(address) {
    const ptr0 = passStringToWasm0(address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshConnectRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshIdentityRequest() {
    const ret = wasm.encodeMeshIdentityRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {string} node_id
 * @param {string} command
 * @param {string[]} args
 * @returns {Uint8Array}
 */
export function encodeMeshNodeCommandRequest(node_id, command, args) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(command, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArrayJsValueToWasm0(args, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshNodeCommandRequest(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeMeshNodeDetailRequest(node_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshNodeDetailRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshNodeListRequest() {
    const ret = wasm.encodeMeshNodeListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {string} node_id
 * @param {string} interface_name
 * @param {string} config_json
 * @returns {Uint8Array}
 */
export function encodeMeshNodeNetworkConfigRequest(node_id, interface_name, config_json) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(interface_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(config_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshNodeNetworkConfigRequest(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
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
 * @param {string} pair_id
 * @param {string} pin
 * @returns {Uint8Array}
 */
export function encodeMeshPairingConfirmRequest(pair_id, pin) {
    const ptr0 = passStringToWasm0(pair_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(pin, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshPairingConfirmRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * @param {string} pair_id
 * @returns {Uint8Array}
 */
export function encodeMeshPairingRejectRequest(pair_id) {
    const ptr0 = passStringToWasm0(pair_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshPairingRejectRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
    const ptr0 = passStringToWasm0(remote_address, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(pin_hint) ? 0 : passStringToWasm0(pin_hint, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(remote_public_key) ? 0 : passStringToWasm0(remote_public_key, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(remote_addresses) ? 0 : passArrayJsValueToWasm0(remote_addresses, wasm.__wbindgen_malloc);
    var len3 = WASM_VECTOR_LEN;
    var ptr4 = isLikeNone(remote_relay_url) ? 0 : passStringToWasm0(remote_relay_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len4 = WASM_VECTOR_LEN;
    var ptr5 = isLikeNone(remote_hostname) ? 0 : passStringToWasm0(remote_hostname, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len5 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshPairingStartRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v7 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v7;
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
 * @returns {Uint8Array}
 */
export function encodeMeshPendingListRequest() {
    const ret = wasm.encodeMeshPendingListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshServicesListRequest() {
    const ret = wasm.encodeMeshServicesListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeMeshTrustRetrustRequest(node_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshTrustRetrustRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeMeshTrustRevokeRequest(node_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeMeshTrustRevokeRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * @returns {Uint8Array}
 */
export function encodeMeshTrustedListRequest() {
    const ret = wasm.encodeMeshTrustedListRequest();
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
 * @param {string} alias
 * @param {string} target_model
 * @param {string | null} [strategy]
 * @param {string | null} [fallback_targets]
 * @returns {Uint8Array}
 */
export function encodeModelAliasCreateRequest(alias, target_model, strategy, fallback_targets) {
    const ptr0 = passStringToWasm0(alias, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(target_model, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(strategy) ? 0 : passStringToWasm0(strategy, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(fallback_targets) ? 0 : passStringToWasm0(fallback_targets, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeModelAliasCreateRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
}

/**
 * @param {number} id
 * @returns {Uint8Array}
 */
export function encodeModelAliasDeleteRequest(id) {
    const ret = wasm.encodeModelAliasDeleteRequest(id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * @returns {Uint8Array}
 */
export function encodeModelAliasListRequest() {
    const ret = wasm.encodeModelAliasListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
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
    const ptr0 = passStringToWasm0(alias, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(target_model, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(strategy) ? 0 : passStringToWasm0(strategy, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(fallback_targets) ? 0 : passStringToWasm0(fallback_targets, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeModelAliasUpdateRequest(id, ptr0, len0, ptr1, len1, isLikeNone(is_active) ? 0xFFFFFF : is_active ? 1 : 0, ptr2, len2, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
}

/**
 * MessageBody::ModelDeleteRequest { model_id }.
 * @param {string} model_id
 * @returns {Uint8Array}
 */
export function encodeModelDeleteRequest(model_id) {
    const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeModelDeleteRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ModelDetailRequest { model_id }.
 * @param {string} model_id
 * @returns {Uint8Array}
 */
export function encodeModelDetailRequest(model_id) {
    const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeModelDetailRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ModelInstallRequest { model_id, source_repo }.
 * @param {string} model_id
 * @param {string} source_repo
 * @returns {Uint8Array}
 */
export function encodeModelInstallRequest(model_id, source_repo) {
    const ptr0 = passStringToWasm0(model_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(source_repo, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeModelInstallRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
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
 * @returns {Uint8Array}
 */
export function encodeModelsUnifiedListRequest() {
    const ret = wasm.encodeModelsUnifiedListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::MyOAuthAccountsListRequest (unit) — lista kont biezacego usera.
 * @returns {Uint8Array}
 */
export function encodeMyOAuthAccountsListRequest() {
    const ret = wasm.encodeMyOAuthAccountsListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqConfigGet).
 * @returns {Uint8Array}
 */
export function encodeNetworkConfigGetRequest() {
    const ret = wasm.encodeNetworkConfigGetRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
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
 * @returns {Uint8Array}
 */
export function encodeNetworkConfigUpdateRequest(bind_mode, bind_ipv4, hide_docker, hide_link_local, hide_loopback, hide_cgnat, prefer_same_subnet, iroh_relay_url) {
    const ptr0 = passStringToWasm0(bind_mode, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(bind_ipv4, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(iroh_relay_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeNetworkConfigUpdateRequest(ptr0, len0, ptr1, len1, hide_docker, hide_link_local, hide_loopback, hide_cgnat, prefer_same_subnet, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqInterfacesList).
 * @returns {Uint8Array}
 */
export function encodeNetworkInterfacesListRequest() {
    const ret = wasm.encodeNetworkInterfacesListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::NetworkBody(NetworkPayload::ReqRelayStatus).
 * @returns {Uint8Array}
 */
export function encodeNetworkRelayStatusRequest() {
    const ret = wasm.encodeNetworkRelayStatusRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::NgcStatusRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeNgcStatusRequest() {
    const ret = wasm.encodeNgcStatusRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::NimCatalogListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeNimCatalogListRequest() {
    const ret = wasm.encodeNimCatalogListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * NotesRequest::Create { title, body }.
 * @param {string} title
 * @param {string} body
 * @returns {Uint8Array}
 */
export function encodeNoteCreateRequest(title, body) {
    const ptr0 = passStringToWasm0(title, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(body, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeNoteCreateRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * NotesRequest::Delete { note_id }.
 * @param {number} note_id
 * @returns {Uint8Array}
 */
export function encodeNoteDeleteRequest(note_id) {
    const ret = wasm.encodeNoteDeleteRequest(note_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * NotesRequest::Detail { note_id }.
 * @param {number} note_id
 * @returns {Uint8Array}
 */
export function encodeNoteDetailRequest(note_id) {
    const ret = wasm.encodeNoteDetailRequest(note_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * NotesRequest::SetPinned { note_id, pinned }.
 * @param {number} note_id
 * @param {boolean} pinned
 * @returns {Uint8Array}
 */
export function encodeNoteSetPinnedRequest(note_id, pinned) {
    const ret = wasm.encodeNoteSetPinnedRequest(note_id, pinned);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * NotesRequest::Update { note_id, title, body }.
 * @param {number} note_id
 * @param {string} title
 * @param {string} body
 * @returns {Uint8Array}
 */
export function encodeNoteUpdateRequest(note_id, title, body) {
    const ptr0 = passStringToWasm0(title, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(body, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeNoteUpdateRequest(note_id, ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * NotesRequest::List — empty inner struct.
 * @returns {Uint8Array}
 */
export function encodeNotesListRequest() {
    const ret = wasm.encodeNotesListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::PiiRuleBody(ListRequest) — wire-compat z dawnym
 * PiiRuleListRequest, JS API niezmienione.
 * @returns {Uint8Array}
 */
export function encodePiiRuleListRequest() {
    const ret = wasm.encodePiiRuleListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ActiveInfoRequest(..)).
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeProfilingActiveInfoRequest(node_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingActiveInfoRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::CollectorsStatusRequest(..)).
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeProfilingCollectorsStatusRequest(node_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingCollectorsStatusRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::DeleteRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingDeleteRequest(node_id, session_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingDeleteRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::DownloadRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingDownloadRequest(node_id, session_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingDownloadRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ReportRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingReportRequest(node_id, session_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingReportRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::SessionsRequest(..)).
 * @param {string} node_id
 * @returns {Uint8Array}
 */
export function encodeProfilingSessionsRequest(node_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingSessionsRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(label, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    var ptr2 = isLikeNone(elevation_password) ? 0 : passStringToWasm0(elevation_password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingStartRequest(ptr0, len0, scope, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::StopRequest(..)).
 * @param {string} node_id
 * @param {string} session_id
 * @returns {Uint8Array}
 */
export function encodeProfilingStopRequest(node_id, session_id) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(session_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingStopRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::ProfilingBody(ProfilingPayload::ValidateSudoRequest(..)).
 * @param {string} node_id
 * @param {string} password
 * @returns {Uint8Array}
 */
export function encodeProfilingValidateSudoRequest(node_id, password) {
    const ptr0 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(password, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeProfilingValidateSudoRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::PromptDetailRequest { prompt_id }.
 * @param {string} prompt_id
 * @returns {Uint8Array}
 */
export function encodePromptDetailRequest(prompt_id) {
    const ptr0 = passStringToWasm0(prompt_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodePromptDetailRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::PromptListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodePromptListRequest() {
    const ret = wasm.encodePromptListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::RegistryListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeRegistryListRequest() {
    const ret = wasm.encodeRegistryListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqDelete) — stop + delete the row
 * (cascades to `model_registry`).
 * @param {number} service_id
 * @param {string | null} [node_id]
 * @returns {Uint8Array}
 */
export function encodeServiceDeleteRequest(service_id, node_id) {
    var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeServiceDeleteRequest(service_id, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqList). Empty filter values are
 * treated as "no filter".
 * @param {string | null} [engine_id_filter]
 * @param {string | null} [category_filter]
 * @returns {Uint8Array}
 */
export function encodeServiceListRequest(engine_id_filter, category_filter) {
    var ptr0 = isLikeNone(engine_id_filter) ? 0 : passStringToWasm0(engine_id_filter, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len0 = WASM_VECTOR_LEN;
    var ptr1 = isLikeNone(category_filter) ? 0 : passStringToWasm0(category_filter, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeServiceListRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
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
    const ptr0 = passStringToWasm0(engine_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(deploy_method, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(config_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeServiceManifestDeployRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
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
    var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeServicePauseRequest(service_id, paused, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
    var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeServicePinRequest(service_id, pinned, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::ServiceBody(ServicePayload::ReqStart) — unpause + spawn the
 * engine when stopped/failed/paused. Idempotent for already-running services.
 * @param {number} service_id
 * @param {string | null} [node_id]
 * @returns {Uint8Array}
 */
export function encodeServiceStartRequest(service_id, node_id) {
    var ptr0 = isLikeNone(node_id) ? 0 : passStringToWasm0(node_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeServiceStartRequest(service_id, ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
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
 * MessageBody::SettingsUpdateRequest — trzy rownolegle tablice (keys/values/is_secrets).
 * Wszystkie 3 musza miec ten sam dlugosc. Pozwala na batch update z JS bez
 * serde-wasm-bindgen.
 * @param {string[]} keys
 * @param {string[]} values
 * @param {Uint8Array} is_secrets
 * @returns {Uint8Array}
 */
export function encodeSettingsUpdateBatch(keys, values, is_secrets) {
    const ptr0 = passArrayJsValueToWasm0(keys, wasm.__wbindgen_malloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArrayJsValueToWasm0(values, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passArray8ToWasm0(is_secrets, wasm.__wbindgen_malloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeSettingsUpdateBatch(ptr0, len0, ptr1, len1, ptr2, len2);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
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
 * MessageBody::SsoProviderCreateRequest — pelne dane providera SSO/OIDC.
 * @param {string} name
 * @param {string} provider_type
 * @param {string} client_id
 * @param {string} client_secret
 * @param {string} discovery_url
 * @param {boolean} auto_create_users
 * @param {number | null} [default_group_id]
 * @returns {Uint8Array}
 */
export function encodeSsoProviderCreateRequest(name, provider_type, client_id, client_secret, discovery_url, auto_create_users, default_group_id) {
    const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(provider_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(client_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ptr3 = passStringToWasm0(client_secret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len3 = WASM_VECTOR_LEN;
    const ptr4 = passStringToWasm0(discovery_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len4 = WASM_VECTOR_LEN;
    const ret = wasm.encodeSsoProviderCreateRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, auto_create_users, !isLikeNone(default_group_id), isLikeNone(default_group_id) ? 0 : default_group_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v6 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v6;
}

/**
 * MessageBody::SsoProviderDeleteRequest { id }.
 * @param {number} id
 * @returns {Uint8Array}
 */
export function encodeSsoProviderDeleteRequest(id) {
    const ret = wasm.encodeSsoProviderDeleteRequest(id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::SsoProvidersListRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeSsoProvidersListRequest() {
    const ret = wasm.encodeSsoProvidersListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
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
 * MessageBody::TlsStatusRequest (unit variant).
 * @returns {Uint8Array}
 */
export function encodeTlsStatusRequest() {
    const ret = wasm.encodeTlsStatusRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
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
    const ptr0 = passStringToWasm0(source_text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(source_lang, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(target_lang, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    var ptr3 = isLikeNone(tone) ? 0 : passStringToWasm0(tone, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    var len3 = WASM_VECTOR_LEN;
    const ret = wasm.encodeTranslateRequest(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v5 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v5;
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
    const ptr0 = passStringToWasm0(id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(pattern, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(voice_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.encodeTtsRuleCreateRequest(ptr0, len0, ptr1, len1, ptr2, len2, priority);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v4;
}

/**
 * MessageBody::TtsRuleDeleteRequest { rule_id }.
 * @param {string} rule_id
 * @returns {Uint8Array}
 */
export function encodeTtsRuleDeleteRequest(rule_id) {
    const ptr0 = passStringToWasm0(rule_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeTtsRuleDeleteRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::TtsRuleListRequest (unit).
 * @returns {Uint8Array}
 */
export function encodeTtsRuleListRequest() {
    const ret = wasm.encodeTtsRuleListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * LEGACY UsersListRequest — zastapione przez encodeIamListUsersRequest.
 * @returns {Uint8Array}
 */
export function encodeUsersListRequest() {
    const ret = wasm.encodeUsersListRequest();
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
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
    const ptr0 = passStringToWasm0(service_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(image, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeVisionInferRequest(ptr0, len0, ptr1, len1, isLikeNone(width) ? 0x100000001 : (width) >>> 0, isLikeNone(height) ? 0x100000001 : (height) >>> 0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
}

/**
 * MessageBody::VncTunnelBody(ReqClose) — tear down tunnel explicitly.
 * @param {string} tunnel_id
 * @returns {Uint8Array}
 */
export function encodeVncTunnelCloseRequest(tunnel_id) {
    const ptr0 = passStringToWasm0(tunnel_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.encodeVncTunnelCloseRequest(ptr0, len0);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v2 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v2;
}

/**
 * MessageBody::VncTunnelBody(ReqOpen) — start streaming tunnel for session.
 * @param {number} session_id
 * @returns {Uint8Array}
 */
export function encodeVncTunnelOpenRequest(session_id) {
    const ret = wasm.encodeVncTunnelOpenRequest(session_id);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v1;
}

/**
 * MessageBody::VncTunnelBody(ReqSend) — browser → container RFB bytes.
 * @param {string} tunnel_id
 * @param {Uint8Array} bytes
 * @returns {Uint8Array}
 */
export function encodeVncTunnelSendRequest(tunnel_id, bytes) {
    const ptr0 = passStringToWasm0(tunnel_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.encodeVncTunnelSendRequest(ptr0, len0, ptr1, len1);
    if (ret[3]) {
        throw takeFromExternrefTable0(ret[2]);
    }
    var v3 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
    wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
    return v3;
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
        __wbg___wbindgen_debug_string_0bc8482c6e3508ae: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_0095a73b8b156f76: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_null_ac34f5003991759a: function(arg0) {
            const ret = arg0 === null;
            return ret;
        },
        __wbg___wbindgen_is_object_5ae8e5880f2c1fbd: function(arg0) {
            const val = arg0;
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_string_cd444516edc5b180: function(arg0) {
            const ret = typeof(arg0) === 'string';
            return ret;
        },
        __wbg___wbindgen_is_undefined_9e4d92534c42d778: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_8ff4255516ccad3e: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
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
        __wbg_call_389efe28435a9388: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.call(arg1);
            return ret;
        }, arguments); },
        __wbg_call_4708e0c13bdc8e95: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_crypto_86f2631e91b51511: function(arg0) {
            const ret = arg0.crypto;
            return ret;
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
        __wbg_from_bddd64e7d5ff6941: function(arg0) {
            const ret = Array.from(arg0);
            return ret;
        },
        __wbg_getItem_0c792d344808dcf5: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg1.getItem(getStringFromWasm0(arg2, arg3));
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_getRandomValues_b3f15fcbfabb0f8b: function() { return handleError(function (arg0, arg1) {
            arg0.getRandomValues(arg1);
        }, arguments); },
        __wbg_get_9b94d73e6221f75c: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_get_b3ed3ad4be2bc8ac: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_instanceof_Window_ed49b2db8df90359: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_isArray_a2cef7634fcb7c0d: function(arg0) {
            const ret = Array.isArray(arg0);
            return ret;
        },
        __wbg_isArray_d314bb98fcf08331: function(arg0) {
            const ret = Array.isArray(arg0);
            return ret;
        },
        __wbg_length_32ed9a279acd054c: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_length_35a7bace40f36eac: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_localStorage_a22d31b9eacc4594: function() { return handleError(function (arg0) {
            const ret = arg0.localStorage;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_msCrypto_d562bbe83e0d4b91: function(arg0) {
            const ret = arg0.msCrypto;
            return ret;
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
        __wbg_new_no_args_1c7c842f08d00ebb: function(arg0, arg1) {
            const ret = new Function(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_with_length_a2c39cbe88fd8ff1: function(arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return ret;
        },
        __wbg_node_e1f24f89a7336c2e: function(arg0) {
            const ret = arg0.node;
            return ret;
        },
        __wbg_process_3975fd6c72f520aa: function(arg0) {
            const ret = arg0.process;
            return ret;
        },
        __wbg_prototypesetcall_bdcdcc5842e4d77d: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_push_8ffdcb2063340ba5: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_randomFillSync_f8c153b79f285817: function() { return handleError(function (arg0, arg1) {
            arg0.randomFillSync(arg1);
        }, arguments); },
        __wbg_removeItem_f6369b1a6fa39850: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.removeItem(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_require_b74f47fc2d022fd6: function() { return handleError(function () {
            const ret = module.require;
            return ret;
        }, arguments); },
        __wbg_setItem_cf340bb2edbd3089: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setItem(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_set_6cb8631f80447a67: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_cc56eefd2dd91957: function(arg0, arg1, arg2) {
            arg0.set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_stack_0ed75d68575b0f3c: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_static_accessor_GLOBAL_12837167ad935116: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_e628e89ab3b1c95f: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_a621d3dfbb60d0ce: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_f8727f0cf888e0bd: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_subarray_a96e1fef17ed23cb: function(arg0, arg1, arg2) {
            const ret = arg0.subarray(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_versions_4e31226f5e8dc909: function(arg0) {
            const ret = arg0.versions;
            return ret;
        },
        __wbindgen_cast_0000000000000001: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Ref(Slice(U8)) -> NamedExternref("Uint8Array")`.
            const ret = getArrayU8FromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0) {
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
