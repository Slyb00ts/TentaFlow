// =============================================================================
// Plik: modules/flows/FlowCanvas.js
// Opis: Canvas SVG do edycji flow - renderowanie wezlow, krawedzi, drag&drop,
//       laczenie portow, zaznaczanie, pan/zoom.
// Przyklad: FlowCanvas.init(svgElement); FlowCanvas.setNodes(nodes);
// =============================================================================

const FlowCanvas = (() => {
  'use strict';

  let svgEl = null;
  let nodesGroup = null;
  let edgesGroup = null;
  let connectingLine = null;

  let nodes = [];
  let edges = [];
  let selectedNodeId = null;
  let selectedEdgeId = null;

  // Mapy szybkiego dostepu do wezlow/krawedzi wg id
  let nodeMap = new Map();
  let edgeMap = new Map();

  // Mapa adjacency: nodeId -> Set<edgeId> (krawedzie polaczone z wezlem)
  let nodeEdgeMap = new Map();

  // Referencje do elementow SVG wezlow/krawedzi - unikamy przebudowy calego DOM
  let nodeElements = new Map();
  let edgeElements = new Map();

  // Stan drag
  let dragState = null;
  let connectState = null;

  // Stan pan/zoom
  let viewBox = { x: 0, y: 0, w: 1200, h: 800 };
  let isPanning = false;
  let panStart = { x: 0, y: 0 };

  // Flaga requestAnimationFrame - zapobieg wielokrotnym renderom w jednej ramce
  let rafId = null;

  // Bufor pozycji drag - aktualizacja DOM tylko w rAF
  let dragRafId = null;
  let dragPending = null;

  // rAF dla linii laczenia portow
  let connectRafId = null;
  let connectPending = null;

  // Cache getBoundingClientRect - invalidowany przy resize
  let rectCache = null;
  let resizeObserver = null;

  // Referencje do poprzednio zaznaczonych elementow (unikamy iteracji po wszystkich)
  let prevSelectedNodeEl = null;
  let prevSelectedEdgePathEl = null;

  // Callback na zmiane
  let onChangeCallback = null;
  let onSelectNodeCallback = null;
  let onSelectEdgeCallback = null;

  // Kolory wezlow wg typu
  const NODE_COLORS = {
    trigger: '#22c55e',
    start: '#22c55e',
    llm: '#6366f1',
    stt: '#f59e0b',
    tts: '#ec4899',
    rag: '#3b82f6',
    memory: '#06b6d4',
    embeddings: '#8b5cf6',
    condition: '#f97316',
    switch: '#a855f7',
    template: '#64748b',
    transform: '#64748b',
    pii_filter: '#10b981',
    tts_clean: '#14b8a6',
    conversation_history: '#06b6d4',
    session_context: '#14b8a6',
    speaker_context: '#f59e0b',
    memory_analyzer: '#8b5cf6',
    router: '#6b7280',
    output: '#ef4444',
    end: '#94a3b8',
  };

  // Opisy typow wezlow (tooltip SVG, referencja dla FlowNodeConfig)
  const NODE_DESCRIPTIONS = {
    trigger: "Flow entry point (HTTP, QUIC, webhook)",
    llm: "LLM call - sends prompt and returns response",
    rag: "Knowledge base search - returns context for LLM",
    stt: "Speech to Text",
    tts: "Text to Speech",
    embeddings: "Text embeddings generation",
    memory: "Conversation memory read/write",
    template: "Text formatting with variables ({input}, {model})",
    pii_filter: "PII removal - uses PII Rules",
    tts_clean: "Text cleaning for TTS - replaces abbreviations (uses TTS Rules)",
    conversation_history: "Conversation history - injects previous messages",
    session_context: "Session awareness - adds session info",
    speaker_context: "Voice recognition - personalization",
    memory_analyzer: "Memory query analysis (bielik-1.5b)",
    condition: "Conditional branch (if/else)",
    switch: "Multiple choice branch",
    router: "Pass-through node",
    output: "Flow output - returns result",
  };

  // Jednolinijkowe opisy I/O na nodach
  const NODE_IO_SUMMARY = {
    trigger: '→ input, model, request_id',
    conversation_history: 'ctx.messages → is_first_message',
    session_context: 'ctx.messages → session_type',
    pii_filter: 'text → text (clean)',
    speaker_context: 'ctx → recognized, person_name',
    memory_analyzer: 'ctx.input → should_query, search_terms',
    condition: 'field → result (true/false)',
    memory: 'search_terms|text → text, memories',
    llm: 'ctx.messages|input → text, tokens',
    tts_clean: 'text → text (clean)',
    rag: 'text → text, sources',
    stt: 'audio → text',
    tts: 'text → audio_base64',
    embeddings: 'text → embedding',
    template: '{input}, {var} → text',
    output: 'text →',
  };

  // Mapa starych polskich nazw do typow (do wstecznej kompatybilnosci tlumaczen)
  const LEGACY_LABELS_MAP = {
    'Trigger': 'trigger',
    'Wywołanie LLM': 'llm',
    'Wyszukiwanie RAG': 'rag',
    'Mowa na tekst': 'stt',
    'Tekst na mowę': 'tts',
    'Embeddingi': 'embeddings',
    'Pamięć kontekstu': 'memory',
    'Szablon tekstu': 'template',
    'Filtr PII': 'pii_filter',
    'Czyszczenie TTS': 'tts_clean',
    'Historia': 'conversation_history',
    'Sesja': 'session_context',
    'Mówca': 'speaker_context',
    'Analizator': 'memory_analyzer',
    'Warunek': 'condition',
    'Switch': 'switch',
    'Router': 'router',
    'Wyjście': 'output'
  };

  // Rozmiary wezla
  const NODE_W = 160;
  const NODE_H = 68;
  const PORT_R = 6;
  const GRID_SIZE = 20;

  // Pobiera wyswietlana nazwe wezla (tlumaczenie lub custom label)
  function getNodeDisplayName(node) {
    const localizedNodeName = I18n.t(`flows.node_names.${node.type}`) || node.type;
    
    // Jesli brak etykiety -> tlumaczenie
    if (!node.label) return localizedNodeName;
    
    // Jesli etykieta jest identyczna z typem -> tlumaczenie
    if (node.label === node.type) return localizedNodeName;

    // Jesli etykieta jest znana jako "stara polska nazwa" dla tego typu -> tlumaczenie
    if (LEGACY_LABELS_MAP[node.label] === node.type) return localizedNodeName;

    // W przeciwnym razie uzytkownik wpisal wlasna nazwe -> zostawiamy
    return node.label;
  }

  // Dynamiczna wysokosc wezla w zaleznosci od typu i konfiguracji
  function getNodeHeight(node) {
    if (node.type === 'condition') return 80;
    if (node.type === 'switch') {
      const cases = (node.config && Array.isArray(node.config.cases)) ? node.config.cases : [];
      return Math.max(68, (cases.length + 1) * 24 + 16);
    }
    return NODE_H;
  }

  // Porty wyjsciowe wezla - wieloporty dla condition/switch
  function getOutputPorts(node) {
    if (node.type === 'condition') {
      return [
        { id: 'true', label: 'T', color: '#22c55e', yOffset: -12 },
        { id: 'false', label: 'F', color: '#ef4444', yOffset: 12 },
      ];
    }
    if (node.type === 'switch') {
      const cases = (node.config && Array.isArray(node.config.cases)) ? node.config.cases : [];
      const ports = [];
      const total = cases.length + 1;
      for (let i = 0; i < cases.length; i++) {
        const yOff = ((i + 1) / (total + 1)) * getNodeHeight(node) - getNodeHeight(node) / 2;
        ports.push({ id: cases[i] || `case_${i}`, label: cases[i] || `${i}`, color: '#a855f7', yOffset: yOff });
      }
      // Port default na dole
      const defY = (total / (total + 1)) * getNodeHeight(node) - getNodeHeight(node) / 2;
      ports.push({ id: 'default', label: '*', color: '#6b7280', yOffset: defY });
      return ports;
    }
    return [{ id: 'default', label: '', color: '', yOffset: 0 }];
  }

  // Inicjalizacja canvasu
  function init(container, onChange, onSelectNode, onSelectEdge) {
    onChangeCallback = onChange;
    onSelectNodeCallback = onSelectNode;
    onSelectEdgeCallback = onSelectEdge;

    svgEl = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svgEl.setAttribute('width', '100%');
    svgEl.setAttribute('height', '100%');
    svgEl.setAttribute('viewBox', `${viewBox.x} ${viewBox.y} ${viewBox.w} ${viewBox.h}`);
    container.appendChild(svgEl);

    // Definicje (znacznik strzalki) - tworzone przez DOM API
    const defs = createSvgEl('defs');
    const marker = createSvgEl('marker', {
      id: 'arrow',
      viewBox: '0 0 10 10',
      refX: '10',
      refY: '5',
      markerWidth: '8',
      markerHeight: '8',
      orient: 'auto-start-reverse',
    });
    const arrowPath = createSvgEl('path', {
      d: 'M 0 0 L 10 5 L 0 10 z',
      fill: 'var(--color-text-muted)',
    });
    marker.appendChild(arrowPath);
    defs.appendChild(marker);
    svgEl.appendChild(defs);

    // Siatka tla
    renderGrid();

    // Grupy warstw
    edgesGroup = createSvgEl('g', { class: 'flow-edges-layer' });
    nodesGroup = createSvgEl('g', { class: 'flow-nodes-layer' });
    svgEl.appendChild(edgesGroup);
    svgEl.appendChild(nodesGroup);

    // Linia laczenia
    connectingLine = createSvgEl('path', { class: 'flow-connecting-line', d: '' });
    connectingLine.style.display = 'none';
    svgEl.appendChild(connectingLine);

    // Zdarzenia
    svgEl.addEventListener('mousedown', handleMouseDown);
    svgEl.addEventListener('mousemove', handleMouseMove);
    svgEl.addEventListener('mouseup', handleMouseUp);
    svgEl.addEventListener('wheel', handleWheel, { passive: false });
    svgEl.addEventListener('click', handleCanvasClick);

    // Invalidacja cache rect przy zmianie rozmiaru
    resizeObserver = new ResizeObserver(() => { rectCache = null; });
    resizeObserver.observe(svgEl);

    return svgEl;
  }

  // Odbudowanie map szybkiego dostepu
  function rebuildMaps() {
    nodeMap.clear();
    for (const n of nodes) nodeMap.set(n.id, n);
    edgeMap.clear();
    nodeEdgeMap.clear();
    for (const e of edges) {
      edgeMap.set(e.id, e);
      addToNodeEdgeMap(e);
    }
  }

  // Dodaje krawedz do mapy adjacency
  function addToNodeEdgeMap(edge) {
    if (!nodeEdgeMap.has(edge.from_node)) nodeEdgeMap.set(edge.from_node, new Set());
    nodeEdgeMap.get(edge.from_node).add(edge.id);
    if (!nodeEdgeMap.has(edge.to_node)) nodeEdgeMap.set(edge.to_node, new Set());
    nodeEdgeMap.get(edge.to_node).add(edge.id);
  }

  // Usuwa krawedz z mapy adjacency
  function removeFromNodeEdgeMap(edge) {
    const fromSet = nodeEdgeMap.get(edge.from_node);
    if (fromSet) fromSet.delete(edge.id);
    const toSet = nodeEdgeMap.get(edge.to_node);
    if (toSet) toSet.delete(edge.id);
  }

  // Ustawianie danych (pelne przeladowanie)
  function setData(newNodes, newEdges) {
    nodes = newNodes || [];
    edges = newEdges || [];
    rebuildMaps();

    // Wyczysc stare elementy SVG
    if (nodesGroup) nodesGroup.textContent = '';
    if (edgesGroup) edgesGroup.textContent = '';
    nodeElements.clear();
    edgeElements.clear();

    renderAll();
  }

  // Pobranie aktualnych danych
  function getData() {
    return { nodes: [...nodes], edges: [...edges] };
  }

  // Dodanie wezla (z palety)
  function addNode(type, label, x, y) {
    const snappedX = Math.round(x / GRID_SIZE) * GRID_SIZE;
    const snappedY = Math.round(y / GRID_SIZE) * GRID_SIZE;

    const node = {
      id: 'node_' + Date.now() + '_' + Math.random().toString(36).substr(2, 4),
      type,
      label: label || type,
      x: snappedX,
      y: snappedY,
      config: {},
    };
    nodes.push(node);
    nodeMap.set(node.id, node);
    scheduleRender();
    selectNode(node.id);
    notifyChange();
    return node;
  }

  // Usuwanie wezla
  function removeNode(nodeId) {
    nodes = nodes.filter(n => n.id !== nodeId);
    nodeMap.delete(nodeId);
    // Znajdz polaczone krawedzie przez mape adjacency
    const connectedEdgeIds = nodeEdgeMap.get(nodeId);
    const removedEdgeIds = connectedEdgeIds ? [...connectedEdgeIds] : [];
    edges = edges.filter(e => e.from_node !== nodeId && e.to_node !== nodeId);
    removedEdgeIds.forEach(id => {
      const edge = edgeMap.get(id);
      if (edge) removeFromNodeEdgeMap(edge);
      edgeMap.delete(id);
    });
    nodeEdgeMap.delete(nodeId);
    if (selectedNodeId === nodeId) {
      selectedNodeId = null;
      prevSelectedNodeEl = null;
    }
    // Usun elementy SVG bezposrednio
    const nodeEl = nodeElements.get(nodeId);
    if (nodeEl && nodeEl.parentNode) nodeEl.parentNode.removeChild(nodeEl);
    nodeElements.delete(nodeId);
    removedEdgeIds.forEach(id => {
      const edgeEl = edgeElements.get(id);
      if (edgeEl && edgeEl.parentNode) edgeEl.parentNode.removeChild(edgeEl);
      edgeElements.delete(id);
    });
    renderEdges();
    notifyChange();
  }

  // Usuwanie krawedzi
  function removeEdge(edgeId) {
    const edge = edgeMap.get(edgeId);
    if (edge) removeFromNodeEdgeMap(edge);
    edges = edges.filter(e => e.id !== edgeId);
    edgeMap.delete(edgeId);
    if (selectedEdgeId === edgeId) {
      selectedEdgeId = null;
      prevSelectedEdgePathEl = null;
    }
    const edgeEl = edgeElements.get(edgeId);
    if (edgeEl && edgeEl.parentNode) edgeEl.parentNode.removeChild(edgeEl);
    edgeElements.delete(edgeId);
    notifyChange();
  }

  // Aktualizacja wezla
  function updateNode(nodeId, updates) {
    const node = nodeMap.get(nodeId);
    if (!node) return;
    Object.assign(node, updates);

    // Przebuduj element SVG jesli to condition/switch (porty mogly sie zmienic)
    if (node.type === 'condition' || node.type === 'switch') {
      const oldEl = nodeElements.get(nodeId);
      if (oldEl && oldEl.parentNode) oldEl.parentNode.removeChild(oldEl);
      nodeElements.delete(nodeId);
    }

    scheduleRender();
    notifyChange();
  }

  // Zaznaczenie wezla - aktualizacja klasy CSS bez przebudowy DOM
  function selectNode(nodeId) {
    selectedNodeId = nodeId;
    selectedEdgeId = null;

    // Zdejmij zaznaczenie z poprzedniego wezla
    if (prevSelectedNodeEl) {
      prevSelectedNodeEl.classList.remove('selected');
      prevSelectedNodeEl = null;
    }
    // Zdejmij zaznaczenie z poprzedniej krawedzi
    if (prevSelectedEdgePathEl) {
      prevSelectedEdgePathEl.classList.remove('selected');
      prevSelectedEdgePathEl = null;
    }
    // Nadaj zaznaczenie nowemu
    if (nodeId && nodeElements.has(nodeId)) {
      const el = nodeElements.get(nodeId);
      el.classList.add('selected');
      prevSelectedNodeEl = el;
    }

    if (onSelectNodeCallback) {
      onSelectNodeCallback(nodeMap.get(nodeId) || null);
    }
  }

  // Zaznaczenie krawedzi - aktualizacja klasy CSS bez przebudowy DOM
  function selectEdge(edgeId) {
    selectedEdgeId = edgeId;
    selectedNodeId = null;

    // Zdejmij zaznaczenie z poprzedniego wezla
    if (prevSelectedNodeEl) {
      prevSelectedNodeEl.classList.remove('selected');
      prevSelectedNodeEl = null;
    }
    // Zdejmij zaznaczenie z poprzedniej krawedzi
    if (prevSelectedEdgePathEl) {
      prevSelectedEdgePathEl.classList.remove('selected');
      prevSelectedEdgePathEl = null;
    }
    // Nadaj zaznaczenie nowej
    if (edgeId && edgeElements.has(edgeId)) {
      const path = edgeElements.get(edgeId).querySelector('.flow-edge');
      if (path) {
        path.classList.add('selected');
        prevSelectedEdgePathEl = path;
      }
    }

    if (onSelectEdgeCallback) {
      onSelectEdgeCallback(edgeMap.get(edgeId) || null);
    }
  }

  // Przeliczenie pozycji myszy na wspolrzedne SVG (z cache rect)
  function toSvgPoint(clientX, clientY) {
    if (!rectCache) rectCache = svgEl.getBoundingClientRect();
    const scaleX = viewBox.w / rectCache.width;
    const scaleY = viewBox.h / rectCache.height;
    return {
      x: (clientX - rectCache.left) * scaleX + viewBox.x,
      y: (clientY - rectCache.top) * scaleY + viewBox.y,
    };
  }

  // Renderowanie wszystkiego
  function renderAll() {
    renderEdges();
    renderNodes();
  }

  // Zaplanuj renderowanie w nastepnej ramce animacji (batch)
  function scheduleRender() {
    if (rafId) return;
    rafId = requestAnimationFrame(() => {
      rafId = null;
      renderAll();
    });
  }

  // Renderowanie siatki
  function renderGrid() {
    // Siatka jest renderowana przez CSS background
  }

  // Tworzenie elementu SVG wezla
  function createNodeElement(node) {
    const h = getNodeHeight(node);

    const g = createSvgEl('g', {
      class: 'flow-node' + (selectedNodeId === node.id ? ' selected' : ''),
      transform: `translate(${node.x}, ${node.y})`,
      'data-node-id': node.id,
    });

    // Tooltip SVG
    const titleEl = createSvgEl('title');
    const localizedNodeName = I18n.t(`flows.node_names.${node.type}`) || node.type;
    const localizedNodeDesc = I18n.t(`flows.node_descriptions.${node.type}`) || node.type;
    const displayName = getNodeDisplayName(node);
    titleEl.textContent = `${displayName}: ${localizedNodeDesc}`;
    g.appendChild(titleEl);

    const color = NODE_COLORS[node.type] || '#64748b';

    const rect = createSvgEl('rect', {
      class: 'node-bg',
      x: 0, y: 0,
      width: NODE_W, height: h,
      fill: color + '20',
      stroke: color,
    });
    g.appendChild(rect);

    const label = createSvgEl('text', {
      class: 'node-label',
      x: NODE_W / 2, y: 22,
      'text-anchor': 'middle',
    });
    label.textContent = displayName;
    g.appendChild(label);

    const typeText = createSvgEl('text', {
      class: 'node-type',
      x: NODE_W / 2, y: 40,
      'text-anchor': 'middle',
    });
    typeText.textContent = node.type;
    g.appendChild(typeText);

    // I/O summary
    const ioText = NODE_IO_SUMMARY[node.type];
    if (ioText) {
      const ioLabel = createSvgEl('text', {
        class: 'node-io-summary',
        x: NODE_W / 2, y: 54,
        'text-anchor': 'middle',
      });
      ioLabel.textContent = ioText;
      g.appendChild(ioLabel);
    }

    // Port wejsciowy
    if (node.type !== 'start' && node.type !== 'trigger') {
      const portIn = createSvgEl('circle', {
        class: 'flow-port flow-port-input',
        cx: 0, cy: h / 2, r: PORT_R,
        'data-port': 'input',
        'data-node-id': node.id,
      });
      g.appendChild(portIn);
    }

    // Porty wyjsciowe (wieloporty dla condition/switch)
    if (node.type !== 'end') {
      const ports = getOutputPorts(node);
      for (const port of ports) {
        const portEl = createSvgEl('circle', {
          class: 'flow-port flow-port-output',
          cx: NODE_W,
          cy: h / 2 + port.yOffset,
          r: PORT_R,
          'data-port': port.id,
          'data-node-id': node.id,
        });
        if (port.color) portEl.setAttribute('fill', port.color);
        g.appendChild(portEl);

        if (port.label) {
          const lbl = createSvgEl('text', {
            class: 'flow-port-label',
            x: NODE_W - 10,
            y: h / 2 + port.yOffset + 4,
            'text-anchor': 'end',
          });
          lbl.textContent = port.label;
          g.appendChild(lbl);
        }
      }
    }

    return g;
  }

  // Aktualizacja istniejacego elementu SVG wezla (pozycja, etykieta, zaznaczenie)
  function updateNodeElement(g, node) {
    g.setAttribute('transform', `translate(${node.x}, ${node.y})`);
    if (selectedNodeId === node.id) {
      g.classList.add('selected');
    } else {
      g.classList.remove('selected');
    }
    const labelEl = g.querySelector('.node-label');
    if (labelEl) {
      const text = getNodeDisplayName(node);
      if (labelEl.textContent !== text) labelEl.textContent = text;
    }
    // Aktualizuj wysokosc
    const h = getNodeHeight(node);
    const rectEl = g.querySelector('.node-bg');
    if (rectEl) rectEl.setAttribute('height', h);
    const portIn = g.querySelector('.flow-port-input');
    if (portIn) portIn.setAttribute('cy', h / 2);
  }

  // Renderowanie wezlow - inkrementalna aktualizacja DOM
  function renderNodes() {
    const currentIds = new Set(nodes.map(n => n.id));

    // Usun wezly ktorych juz nie ma
    for (const [id, el] of nodeElements) {
      if (!currentIds.has(id)) {
        if (el.parentNode) el.parentNode.removeChild(el);
        nodeElements.delete(id);
      }
    }

    // Dodaj nowe lub aktualizuj istniejace
    for (const node of nodes) {
      let g = nodeElements.get(node.id);
      if (g) {
        updateNodeElement(g, node);
      } else {
        g = createNodeElement(node);
        nodesGroup.appendChild(g);
        nodeElements.set(node.id, g);
      }
    }
  }

  // Obliczenie sciezki krzywej Beziera miedzy dwoma wezlami (z uwzglednieniem portu)
  function computeEdgePath(fromNode, toNode, edge) {
    const fromH = getNodeHeight(fromNode);
    const toH = getNodeHeight(toNode);

    let fromYOffset = 0;
    if (edge && edge.from_port) {
      const ports = getOutputPorts(fromNode);
      const p = ports.find(p => p.id === edge.from_port);
      if (p) fromYOffset = p.yOffset;
    }

    const x1 = fromNode.x + NODE_W;
    const y1 = fromNode.y + fromH / 2 + fromYOffset;
    const x2 = toNode.x;
    const y2 = toNode.y + toH / 2;
    const dx = Math.abs(x2 - x1) * 0.5;
    return {
      d: `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`,
      x1, y1, x2, y2,
    };
  }

  // Renderowanie krawedzi - inkrementalna aktualizacja DOM
  function renderEdges() {
    const currentIds = new Set(edges.map(e => e.id));

    // Usun krawedzie ktorych juz nie ma
    for (const [id, el] of edgeElements) {
      if (!currentIds.has(id)) {
        if (el.parentNode) el.parentNode.removeChild(el);
        edgeElements.delete(id);
      }
    }

    for (const edge of edges) {
      const fromNode = nodeMap.get(edge.from_node);
      const toNode = nodeMap.get(edge.to_node);
      if (!fromNode || !toNode) continue;

      const { d, x1, y1, x2, y2 } = computeEdgePath(fromNode, toNode, edge);

      let g = edgeElements.get(edge.id);
      if (g) {
        // Aktualizuj istniejaca krawedz (tylko sciezke i zaznaczenie)
        const path = g.querySelector('.flow-edge');
        if (path) {
          path.setAttribute('d', d);
          if (selectedEdgeId === edge.id) {
            path.classList.add('selected');
          } else {
            path.classList.remove('selected');
          }
        }
        // Aktualizuj etykiete
        const label = g.querySelector('.flow-edge-label');
        if (label && edge.from_port) {
          label.setAttribute('x', (x1 + x2) / 2);
          label.setAttribute('y', (y1 + y2) / 2 - 8);
        }
      } else {
        // Stworz nowy element krawedzi
        g = createSvgEl('g', { class: 'flow-edge-group' });

        const path = createSvgEl('path', {
          class: 'flow-edge' + (selectedEdgeId === edge.id ? ' selected' : '') + (edge.condition ? ' conditional' : ''),
          d,
          'marker-end': 'url(#arrow)',
          'data-edge-id': edge.id,
        });
        g.appendChild(path);

        if (edge.from_port) {
          const mx = (x1 + x2) / 2;
          const my = (y1 + y2) / 2 - 8;
          const label = createSvgEl('text', {
            class: 'flow-edge-label',
            x: mx, y: my,
            'text-anchor': 'middle',
          });
          label.textContent = edge.from_port;
          g.appendChild(label);
        }

        edgesGroup.appendChild(g);
        edgeElements.set(edge.id, g);
      }
    }
  }

  // Obsluga klikniecia na canvas (odznaczenie)
  function handleCanvasClick(e) {
    if (e.target === svgEl || (e.target.tagName === 'rect' && e.target.parentNode === svgEl)) {
      // Odznacz wezel
      if (prevSelectedNodeEl) {
        prevSelectedNodeEl.classList.remove('selected');
        prevSelectedNodeEl = null;
      }
      // Odznacz krawedz
      if (prevSelectedEdgePathEl) {
        prevSelectedEdgePathEl.classList.remove('selected');
        prevSelectedEdgePathEl = null;
      }
      selectedNodeId = null;
      selectedEdgeId = null;
      if (onSelectNodeCallback) onSelectNodeCallback(null);
      return;
    }

    // Klikniecie na krawedz
    const edgePath = e.target.closest('.flow-edge');
    if (edgePath) {
      const edgeId = edgePath.dataset.edgeId;
      if (edgeId) selectEdge(edgeId);
    }
  }

  // Obsluga myszy - poczatek
  function handleMouseDown(e) {
    const pt = toSvgPoint(e.clientX, e.clientY);

    // Laczenie portow - z konkretnego portu
    const portEl = e.target.closest('.flow-port-output');
    if (portEl) {
      const nodeId = portEl.dataset.nodeId;
      connectState = { fromNodeId: nodeId, fromPort: portEl.dataset.port || 'default', startX: pt.x, startY: pt.y };
      connectingLine.style.display = '';
      e.preventDefault();
      return;
    }

    // Przeciaganie wezla
    const nodeEl = e.target.closest('.flow-node');
    if (nodeEl) {
      const nodeId = nodeEl.dataset.nodeId;
      const node = nodeMap.get(nodeId);
      if (node) {
        dragState = {
          nodeId,
          offsetX: pt.x - node.x,
          offsetY: pt.y - node.y,
        };
        selectNode(nodeId);
        e.preventDefault();
        return;
      }
    }

    // Panoramowanie
    isPanning = true;
    panStart.x = e.clientX;
    panStart.y = e.clientY;
  }

  // Szybka aktualizacja tylko jednego wezla i polaczonych krawedzi (drag)
  function updateSingleNode(nodeId) {
    const node = nodeMap.get(nodeId);
    if (!node) return;

    const g = nodeElements.get(nodeId);
    if (g) {
      g.setAttribute('transform', `translate(${node.x}, ${node.y})`);
    }

    // Zaktualizuj tylko krawedzie polaczone z tym wezlem (przez mape adjacency)
    const connectedEdgeIds = nodeEdgeMap.get(nodeId);
    if (connectedEdgeIds) {
      for (const edgeId of connectedEdgeIds) {
        const edge = edgeMap.get(edgeId);
        if (!edge) continue;

        const fromNode = nodeMap.get(edge.from_node);
        const toNode = nodeMap.get(edge.to_node);
        if (!fromNode || !toNode) continue;

        const eg = edgeElements.get(edge.id);
        if (!eg) continue;

        const { d, x1, y1, x2, y2 } = computeEdgePath(fromNode, toNode, edge);
        const path = eg.querySelector('.flow-edge');
        if (path) path.setAttribute('d', d);

        const label = eg.querySelector('.flow-edge-label');
        if (label) {
          label.setAttribute('x', (x1 + x2) / 2);
          label.setAttribute('y', (y1 + y2) / 2 - 8);
        }
      }
    }
  }

  // Realizacja buforowanego dragu w ramce animacji
  function flushDrag() {
    dragRafId = null;
    if (!dragState || !dragPending) return;
    const pt = toSvgPoint(dragPending.clientX, dragPending.clientY);
    dragPending = null;
    const node = nodeMap.get(dragState.nodeId);
    if (node) {
      node.x = Math.round((pt.x - dragState.offsetX) / GRID_SIZE) * GRID_SIZE;
      node.y = Math.round((pt.y - dragState.offsetY) / GRID_SIZE) * GRID_SIZE;
      updateSingleNode(dragState.nodeId);
    }
  }

  // Referencja do ostatniego drop-target portu
  let lastDropTarget = null;

  // Realizacja buforowanej linii laczenia w ramce animacji
  function flushConnect() {
    connectRafId = null;
    if (!connectState || !connectPending) return;
    const pt = toSvgPoint(connectPending.clientX, connectPending.clientY);
    const fromNode = nodeMap.get(connectState.fromNodeId);
    if (fromNode) {
      const fromH = getNodeHeight(fromNode);
      let fromYOffset = 0;
      if (connectState.fromPort) {
        const ports = getOutputPorts(fromNode);
        const p = ports.find(p => p.id === connectState.fromPort);
        if (p) fromYOffset = p.yOffset;
      }
      const x1 = fromNode.x + NODE_W;
      const y1 = fromNode.y + fromH / 2 + fromYOffset;
      const dx = Math.abs(pt.x - x1) * 0.5;
      const d = `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${pt.x - dx} ${pt.y}, ${pt.x} ${pt.y}`;
      connectingLine.setAttribute('d', d);
    }

    // Podswietl port docelowy
    const portEl = document.elementFromPoint(connectPending.clientX, connectPending.clientY);
    connectPending = null;
    if (lastDropTarget && lastDropTarget !== portEl) {
      lastDropTarget.classList.remove('flow-port-drop-target');
    }
    if (portEl && portEl.classList.contains('flow-port-input')) {
      portEl.classList.add('flow-port-drop-target');
      lastDropTarget = portEl;
    } else {
      lastDropTarget = null;
    }
  }

  // Obsluga myszy - ruch (z requestAnimationFrame)
  function handleMouseMove(e) {
    // Laczenie - buforuj pozycje i aktualizuj DOM w rAF
    if (connectState) {
      connectPending = { clientX: e.clientX, clientY: e.clientY };
      if (!connectRafId) {
        connectRafId = requestAnimationFrame(flushConnect);
      }
      return;
    }

    // Przeciaganie - buforuj pozycje i aktualizuj DOM w rAF
    if (dragState) {
      dragPending = { clientX: e.clientX, clientY: e.clientY };
      if (!dragRafId) {
        dragRafId = requestAnimationFrame(flushDrag);
      }
      return;
    }

    // Panoramowanie
    if (isPanning) {
      if (!rectCache) rectCache = svgEl.getBoundingClientRect();
      const scaleX = viewBox.w / rectCache.width;
      const scaleY = viewBox.h / rectCache.height;
      viewBox.x -= (e.clientX - panStart.x) * scaleX;
      viewBox.y -= (e.clientY - panStart.y) * scaleY;
      panStart.x = e.clientX;
      panStart.y = e.clientY;
      svgEl.setAttribute('viewBox', `${viewBox.x} ${viewBox.y} ${viewBox.w} ${viewBox.h}`);
    }
  }

  // Obsluga myszy - koniec
  function handleMouseUp(e) {
    // Zakonczenie laczenia
    if (connectState) {
      if (connectRafId) {
        cancelAnimationFrame(connectRafId);
        connectRafId = null;
      }
      connectPending = null;
      const portEl = document.elementFromPoint(e.clientX, e.clientY);
      if (portEl && portEl.classList.contains('flow-port-input')) {
        const toNodeId = portEl.dataset.nodeId;
        if (toNodeId && toNodeId !== connectState.fromNodeId) {
          const exists = edges.some(ed =>
            ed.from_node === connectState.fromNodeId && ed.to_node === toNodeId
          );
          if (!exists) {
            const newEdge = {
              id: 'edge_' + Date.now(),
              from_node: connectState.fromNodeId,
              to_node: toNodeId,
              from_port: connectState.fromPort || 'default',
            };
            edges.push(newEdge);
            edgeMap.set(newEdge.id, newEdge);
            addToNodeEdgeMap(newEdge);
            notifyChange();
          }
        }
      }
      if (lastDropTarget) {
        lastDropTarget.classList.remove('flow-port-drop-target');
        lastDropTarget = null;
      }
      connectingLine.style.display = 'none';
      connectState = null;
      renderEdges();
    }

    // Zakonczenie przeciagania - flush buforowanej pozycji
    if (dragState) {
      if (dragRafId) {
        cancelAnimationFrame(dragRafId);
        dragRafId = null;
      }
      if (dragPending) flushDrag();
      notifyChange();
      dragState = null;
    }

    isPanning = false;
  }

  // Obsluga scrolla (zoom) z limitami min/max
  function handleWheel(e) {
    e.preventDefault();
    const factor = e.deltaY > 0 ? 1.1 : 0.9;
    const pt = toSvgPoint(e.clientX, e.clientY);

    let newW = viewBox.w * factor;
    let newH = viewBox.h * factor;

    // Limity zoomu
    const MIN_SIZE = 200;
    const MAX_SIZE = 10000;
    newW = Math.max(MIN_SIZE, Math.min(MAX_SIZE, newW));
    newH = Math.max(MIN_SIZE, Math.min(MAX_SIZE, newH));

    const actualFactorW = newW / viewBox.w;
    const actualFactorH = newH / viewBox.h;

    viewBox.x = pt.x - (pt.x - viewBox.x) * actualFactorW;
    viewBox.y = pt.y - (pt.y - viewBox.y) * actualFactorH;
    viewBox.w = newW;
    viewBox.h = newH;

    svgEl.setAttribute('viewBox', `${viewBox.x} ${viewBox.y} ${viewBox.w} ${viewBox.h}`);
  }

  // Powiadomienie o zmianie
  function notifyChange() {
    if (onChangeCallback) onChangeCallback();
  }

  // Pomocnik tworzenia elementu SVG
  function createSvgEl(tag, attrs) {
    const el = document.createElementNS('http://www.w3.org/2000/svg', tag);
    if (attrs) {
      for (const [k, v] of Object.entries(attrs)) {
        el.setAttribute(k, v);
      }
    }
    return el;
  }

  // Obsluga drop z palety
  function handleDrop(type, label, clientX, clientY) {
    const pt = toSvgPoint(clientX, clientY);
    return addNode(type, label, pt.x - NODE_W / 2, pt.y - NODE_H / 2);
  }

  // Usuniecie zaznaczonego elementu
  function deleteSelected() {
    if (selectedNodeId) {
      removeNode(selectedNodeId);
    } else if (selectedEdgeId) {
      removeEdge(selectedEdgeId);
    }
  }

  // Zniszczenie canvasu
  function destroy() {
    if (rafId) {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
    if (dragRafId) {
      cancelAnimationFrame(dragRafId);
      dragRafId = null;
    }
    if (connectRafId) {
      cancelAnimationFrame(connectRafId);
      connectRafId = null;
    }
    dragPending = null;
    connectPending = null;
    if (resizeObserver) {
      resizeObserver.disconnect();
      resizeObserver = null;
    }
    rectCache = null;
    if (svgEl) {
      svgEl.removeEventListener('mousedown', handleMouseDown);
      svgEl.removeEventListener('mousemove', handleMouseMove);
      svgEl.removeEventListener('mouseup', handleMouseUp);
      svgEl.removeEventListener('wheel', handleWheel);
      svgEl.removeEventListener('click', handleCanvasClick);
      if (svgEl.parentNode) svgEl.parentNode.removeChild(svgEl);
    }
    svgEl = null;
    nodes = [];
    edges = [];
    nodeMap.clear();
    edgeMap.clear();
    nodeEdgeMap.clear();
    nodeElements.clear();
    edgeElements.clear();
    selectedNodeId = null;
    selectedEdgeId = null;
    prevSelectedNodeEl = null;
    prevSelectedEdgePathEl = null;
    dragState = null;
    connectState = null;
    isPanning = false;
    lastDropTarget = null;
  }

  return {
    init,
    setData,
    getData,
    addNode,
    removeNode,
    removeEdge,
    updateNode,
    selectNode,
    handleDrop,
    deleteSelected,
    destroy,
    NODE_DESCRIPTIONS,
  };
})();
