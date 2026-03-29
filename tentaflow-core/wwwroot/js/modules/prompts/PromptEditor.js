// =============================================================================
// Plik: modules/prompts/PromptEditor.js
// Opis: Modal edytora promptow - formularz tworzenia/edycji z wykrywaniem
//       zmiennych, podgladem i sliderem priorytetu cache.
// Przyklad: PromptEditor.open(null, onSaved); // nowy prompt
//           PromptEditor.open(promptData, onSaved); // edycja
// =============================================================================

const PromptEditor = (() => {
  'use strict';

  let overlay = null;
  let currentPrompt = null;
  let onSaveCallback = null;

  // Otwarcie modala
  function open(prompt, onSaved) {
    currentPrompt = prompt;
    onSaveCallback = onSaved;
    createModal();
  }

  // Tworzenie modala w DOM
  function createModal() {
    // Usun poprzedni modal bez resetowania stanu (close() kasuje currentPrompt)
    if (overlay && overlay.parentNode) {
      overlay.parentNode.removeChild(overlay);
    }
    overlay = null;

    const isEdit = !!currentPrompt;
    const title = isEdit ? I18n.t('common.edit') : I18n.t('prompts.editor.created').replace('Prompt "{name}" created', 'New prompt');

    // Parsowanie zmiennych z aktualnej tresci
    const existingVars = currentPrompt ? extractVariables(currentPrompt.content || '') : [];

    overlay = document.createElement('div');
    overlay.className = 'modal-overlay active';
    overlay.innerHTML = `
      <div class="modal prompt-editor-modal">
        <div class="modal-header">
          <h3>${title}</h3>
          <button class="modal-close" id="pe-close-btn">&times;</button>
        </div>
        <div class="modal-body">
          <form id="prompt-form">
            <div class="form-group">
              <label for="pe-prompt-id" data-i18n="prompts.editor.slug">${I18n.t('prompts.editor.slug')}</label>
              <input type="text" id="pe-prompt-id" placeholder="np. jarvis_system"
                ${isEdit ? 'readonly' : ''}
                value="${Utils.escapeAttr(currentPrompt?.prompt_id || '')}">
              <div class="form-hint" data-i18n="prompts.editor.slug_hint">${I18n.t('prompts.editor.slug_hint')}</div>
            </div>

            <div class="form-group">
              <label for="pe-name" data-i18n="common.name">${I18n.t('common.name')}</label>
              <input type="text" id="pe-name" placeholder="e.g. Main system prompt" required
                value="${Utils.escapeAttr(currentPrompt?.name || '')}">
            </div>

            <div class="form-group">
              <label for="pe-description" data-i18n="common.description">${I18n.t('common.description')}</label>
              <textarea id="pe-description" rows="2" placeholder="${I18n.t('common.description')}"></textarea>
            </div>

            <div class="prompt-editor-row">
              <div class="form-group" style="flex: 1;">
                <label for="pe-type" data-i18n="prompts.editor.type">${I18n.t('prompts.editor.type')}</label>
                <select id="pe-type">
                  <option value="system" ${sel('system')}>System</option>
                  <option value="suffix" ${sel('suffix')}>Suffix</option>
                  <option value="template" ${sel('template')}>Template</option>
                  <option value="user" ${sel('user')}>User</option>
                </select>
              </div>
              <div class="form-group" style="flex: 1;">
                <label for="pe-model" data-i18n="prompts.editor.default_model">${I18n.t('prompts.editor.default_model')}</label>
                <input type="text" id="pe-model" placeholder="np. bielik-11b"
                  value="${Utils.escapeAttr(currentPrompt?.default_model || '')}">
              </div>
            </div>

            <div class="form-group">
              <label for="pe-content" data-i18n="prompts.editor.content">${I18n.t('prompts.editor.content')}</label>
              <textarea id="pe-content" class="prompt-content-area"
                placeholder="...">${Utils.escapeHtml(currentPrompt?.content || '')}</textarea>
            </div>

            <div class="form-group">
              <label data-i18n="prompts.editor.vars">${I18n.t('prompts.editor.vars')}</label>
              <div id="pe-vars-list" class="prompt-vars-list">
                ${existingVars.map(v => `<span class="prompt-var-tag">{${Utils.escapeHtml(v)}}</span>`).join('') || `<span style="color: var(--color-text-muted); font-size: var(--font-size-sm);" data-i18n="prompts.editor.no_vars">${I18n.t('prompts.editor.no_vars')}</span>`}
              </div>
            </div>

            <div class="form-group">
              <label for="pe-cache" data-i18n="prompts.editor.priority">${I18n.t('prompts.editor.priority')}: <span id="pe-cache-value">${currentPrompt?.cache_priority ?? 50}</span></label>
              <input type="range" id="pe-cache" class="prompt-cache-slider" min="0" max="100" step="1"
                value="${currentPrompt?.cache_priority ?? 50}">
              <div class="prompt-cache-labels">
                <span>0 (${I18n.t('common.unknown').replace('Unknown', 'Low')})</span>
                <span>100 (${I18n.t('common.unknown').replace('Unknown', 'High')})</span>
              </div>
            </div>

            <div class="form-group">
              <label class="prompt-toggle-label">
                <input type="checkbox" id="pe-active" ${currentPrompt?.is_active !== false ? 'checked' : ''}>
                <span data-i18n="common.active">${I18n.t('common.active')}</span>
              </label>
            </div>

            <div class="prompt-preview-section">
              <label data-i18n="common.preview">${I18n.t('common.preview')}</label>
              <div id="pe-preview" class="prompt-preview">
                <span style="color: var(--color-text-muted);" data-i18n="prompts.editor.preview_hint">${I18n.t('prompts.editor.preview_hint')}</span>
              </div>
            </div>

            <div id="pe-form-error" class="form-error" hidden></div>
          </form>
        </div>
        <div class="modal-footer">
          <button class="btn btn-ghost btn-sm" id="pe-test-btn" data-i18n="settings.portainer.test">${I18n.t('settings.portainer.test')}</button>
          <div style="flex: 1;"></div>
          <button class="btn btn-secondary" id="pe-cancel-btn" data-i18n="common.cancel">${I18n.t('common.cancel')}</button>
          <button class="btn btn-primary" id="pe-save-btn">${isEdit ? I18n.t('common.save') : I18n.t('common.add')}</button>
        </div>
      </div>
    `;

    document.body.appendChild(overlay);
    mountEvents();
    updatePreview();
  }

  // Podepnij zdarzenia modala
  function mountEvents() {
    overlay.querySelector('#pe-close-btn').addEventListener('click', close);
    overlay.querySelector('#pe-cancel-btn').addEventListener('click', close);
    overlay.querySelector('#pe-save-btn').addEventListener('click', handleSave);
    overlay.querySelector('#pe-test-btn').addEventListener('click', handleTest);
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) close();
    });

    // Wykrywanie zmiennych w tresci (debounce 200ms)
    const contentArea = overlay.querySelector('#pe-content');
    if (contentArea) {
      let contentTimeout = null;
      contentArea.addEventListener('input', () => {
        clearTimeout(contentTimeout);
        contentTimeout = setTimeout(() => {
          updateVariablesList();
          updatePreview();
        }, 200);
      });
    }

    // Slider cache
    const cacheSlider = overlay.querySelector('#pe-cache');
    if (cacheSlider) {
      cacheSlider.addEventListener('input', () => {
        const label = overlay.querySelector('#pe-cache-value');
        if (label) label.textContent = cacheSlider.value;
      });
    }

    // Auto-generowanie slug z nazwy (tylko dla nowego promptu)
    if (!currentPrompt) {
      const nameInput = overlay.querySelector('#pe-name');
      const slugInput = overlay.querySelector('#pe-prompt-id');
      if (nameInput && slugInput) {
        nameInput.addEventListener('input', () => {
          if (!slugInput.dataset.manual) {
            slugInput.value = generateSlug(nameInput.value);
          }
        });
        slugInput.addEventListener('input', () => {
          slugInput.dataset.manual = 'true';
        });
      }
    }
  }

  // Zamkniecie modala
  function close() {
    if (overlay && overlay.parentNode) {
      overlay.parentNode.removeChild(overlay);
    }
    overlay = null;
    currentPrompt = null;
  }

  // Wyciagniecie zmiennych z tresci - regex {zmienna}
  function extractVariables(text) {
    const matches = text.match(/\{(\w+)\}/g);
    if (!matches) return [];
    const vars = matches.map(m => m.slice(1, -1));
    return [...new Set(vars)];
  }

  // Aktualizacja listy wykrytych zmiennych
  function updateVariablesList() {
    const content = overlay.querySelector('#pe-content')?.value || '';
    const container = overlay.querySelector('#pe-vars-list');
    if (!container) return;

    const vars = extractVariables(content);
    if (vars.length === 0) {
      container.innerHTML = `<span style="color: var(--color-text-muted); font-size: var(--font-size-sm);" data-i18n="prompts.editor.no_vars">${I18n.t('prompts.editor.no_vars')}</span>`;
    } else {
      container.innerHTML = vars.map(v => `<span class="prompt-var-tag">{${Utils.escapeHtml(v)}}</span>`).join('');
    }
  }

  // Aktualizacja podgladu tresci z przykladowymi wartosciami zmiennych
  function updatePreview() {
    const content = overlay.querySelector('#pe-content')?.value || '';
    const previewEl = overlay.querySelector('#pe-preview');
    if (!previewEl) return;

    if (!content.trim()) {
      previewEl.innerHTML = `<span style="color: var(--color-text-muted);" data-i18n="prompts.editor.preview_hint">${I18n.t('prompts.editor.preview_hint')}</span>`;
      return;
    }

    // Zamien zmienne na przykladowe wartosci
    const exampleValues = {
      name: 'John Doe',
      context: '[conversation context]',
      language: 'English',
      topic: '[topic]',
      role: '[role]',
      task: '[task]',
      format: '[format]',
      input: '[input data]',
      output: '[output data]',
    };

    let preview = Utils.escapeHtml(content);
    const vars = extractVariables(content);
    vars.forEach(v => {
      const example = exampleValues[v] || `[${v}]`;
      const regex = new RegExp(`\\{${v}\\}`, 'g');
      preview = preview.replace(regex, `<span class="prompt-var-highlight">${Utils.escapeHtml(example)}</span>`);
    });

    previewEl.innerHTML = preview.replace(/\n/g, '<br>');
  }

  // Obsluga zapisu
  async function handleSave() {
    const promptId = overlay.querySelector('#pe-prompt-id')?.value.trim();
    const name = overlay.querySelector('#pe-name')?.value.trim();
    const description = overlay.querySelector('#pe-description')?.value.trim() || null;
    const promptType = overlay.querySelector('#pe-type')?.value;
    const defaultModel = overlay.querySelector('#pe-model')?.value.trim() || null;
    const content = overlay.querySelector('#pe-content')?.value || '';
    const cachePriority = parseInt(overlay.querySelector('#pe-cache')?.value || '50', 10);
    const isActive = overlay.querySelector('#pe-active')?.checked ? 1 : 0;

    // Walidacja
    if (!promptId) {
      showFormError(I18n.t('prompts.editor.slug') + ' ' + I18n.t('common.required').toLowerCase());
      return;
    }

    if (!/^[a-z0-9_]+$/.test(promptId)) {
      showFormError(I18n.t('prompts.editor.slug_hint'));
      return;
    }

    if (!name) {
      showFormError(I18n.t('rules.pii.name_required'));
      return;
    }

    if (!content.trim()) {
      showFormError(I18n.t('rules.pii.pattern_required'));
      return;
    }

    const vars = extractVariables(content);
    const variablesJson = JSON.stringify(vars);

    const saveBtn = overlay.querySelector('#pe-save-btn');
    if (saveBtn) {
      saveBtn.disabled = true;
      saveBtn.textContent = '...';
    }

    try {
      const payload = {
        prompt_id: promptId,
        name,
        description,
        content,
        prompt_type: promptType,
        default_model: defaultModel,
        variables: variablesJson,
        cache_priority: cachePriority,
        is_active: isActive,
      };

      if (currentPrompt) {
        payload.id = currentPrompt.id;
        await ApiClient.put(`/api/prompts/${currentPrompt.id}`, payload);
        App.showToast(I18n.t('prompts.editor.updated').replace('{name}', name), 'success');
      } else {
        await ApiClient.post('/api/prompts', payload);
        App.showToast(I18n.t('prompts.editor.created').replace('{name}', name), 'success');
      }

      close();
      if (onSaveCallback) onSaveCallback();
    } catch (err) {
      showFormError(err.message || I18n.t('common.error'));
    } finally {
      if (saveBtn) {
        saveBtn.disabled = false;
        saveBtn.textContent = currentPrompt ? I18n.t('common.save') : I18n.t('common.add');
      }
    }
  }

  // Placeholder testowania promptu
  function handleTest() {
    alert(I18n.t('common.unknown').replace('Unknown', 'Feature coming soon'));
  }

  // Generowanie slug z nazwy
  function generateSlug(name) {
    const polishMap = {
      'ą': 'a', 'ć': 'c', 'ę': 'e', 'ł': 'l', 'ń': 'n',
      'ó': 'o', 'ś': 's', 'ź': 'z', 'ż': 'z',
      'Ą': 'A', 'Ć': 'C', 'Ę': 'E', 'Ł': 'L', 'Ń': 'N',
      'Ó': 'O', 'Ś': 'S', 'Ź': 'Z', 'Ż': 'Z'
    };
    let str = name.toLowerCase();
    str = str.replace(/[ąćęłńóśźżĄĆĘŁŃÓŚŹŻ]/g, c => polishMap[c] || c);
    return str
      .replace(/[^a-z0-9]+/g, '_')
      .replace(/^_+|_+$/g, '')
      .substring(0, 64);
  }

  // Wyswietlenie bledu w formularzu
  function showFormError(message) {
    const el = overlay?.querySelector('#pe-form-error');
    if (el) {
      el.textContent = message;
      el.hidden = false;
    }
  }

  // Pomocnik selected
  function sel(value) {
    return currentPrompt?.prompt_type === value ? 'selected' : '';
  }

  return { open, close };
})();
