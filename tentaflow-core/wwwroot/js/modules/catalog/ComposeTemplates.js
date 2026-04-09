// =============================================================================
// Plik: modules/catalog/ComposeTemplates.js
// Opis: Generatory szablonow Docker Compose YAML per usluga.
// =============================================================================

const ComposeTemplates = (() => {
  'use strict';

  // Bazowe sciezki i stale
  const BASE_PATHS = {
    config: '/opt/tentaflow',
    certs: '/opt/tentaflow/certs',
    models: '/opt/tentaflow/models',
    registry: 'registry.nextapp.pl',
    network: 'tentaflow-ai',
  };

  // Wewnetrzne porty uslug (porty na ktorych nasluchuje proces w kontenerze)
  const INTERNAL_PORTS = {
    tts: 5000,
    stt: 5000,
    rag: 5000,
    embeddings: 5000,
    tools: 5000,
    memory: 5002,
    reranker: 5000,
    'meeting-bot': 5000,
    llm: 5010,
    llm_quic: 5000,
    tentaflow: 8443,
    bms: 8444,
    bms_emise: 11014,
  };

  // Flaga GPU per usluga
  const GPU_SERVICES = {
    tts: true,
    stt: true,
    embeddings: true,
    rag: false,
    tools: false,
    memory: false,
    reranker: true,
    'meeting-bot': false,
    tentaflow: false,
    bms: false,
  };

  // Generowanie sekcji GPU deploy
  function gpuSection(gpuId, indent) {
    const pad = ' '.repeat(indent);
    if (gpuId === 'all') {
      return [
        `${pad}deploy:`,
        `${pad}  resources:`,
        `${pad}    reservations:`,
        `${pad}      devices:`,
        `${pad}        - driver: nvidia`,
        `${pad}          count: all`,
        `${pad}          capabilities: [gpu]`,
      ].join('\n');
    }
    return [
      `${pad}deploy:`,
      `${pad}  resources:`,
      `${pad}    reservations:`,
      `${pad}      devices:`,
      `${pad}        - driver: nvidia`,
      `${pad}          device_ids: ['${gpuId}']`,
      `${pad}          capabilities: [gpu]`,
    ].join('\n');
  }

  // Sekcja networks na koncu pliku
  function networksSection() {
    return [
      '',
      'networks:',
      `  ${BASE_PATHS.network}:`,
      `    name: ${BASE_PATHS.network}`,
    ].join('\n');
  }

  // Domyslna sciezka katalogu konfiguracji uslugi
  function defaultConfigDir(serviceId) {
    return `${BASE_PATHS.config}/${serviceId}`;
  }

  // Generowanie compose dla standardowych uslug (nie-LLM)
  function generateService(serviceId, params) {
    const port = params.port || INTERNAL_PORTS[serviceId];
    const internalPort = INTERNAL_PORTS[serviceId];
    const configDir = params.configDir || defaultConfigDir(serviceId);
    const gpuId = params.gpuId || '0';
    const hasGpu = GPU_SERVICES[serviceId];

    // Wolumeny bazowe - TTS/STT maja wbudowane modele i config w obrazie
    const volumes = [];
    if (serviceId !== 'tts' && serviceId !== 'stt') {
      volumes.push(`      - ${configDir}:/data`);
    }
    volumes.push(`      - ${BASE_PATHS.certs}:/data/certs:ro`);

    // Dodatkowe wolumeny per usluga
    if (serviceId === 'memory') {
      volumes.push(`      - ${BASE_PATHS.config}/memory/data:/app/data`);
    }
    if (serviceId === 'embeddings' || serviceId === 'reranker') {
      volumes.push(`      - ${BASE_PATHS.models}:/data/models`);
    }

    const hfToken = params.hfToken || '';

    let yaml = [
      'services:',
      `  tentaflow-${serviceId}:`,
      `    image: ${BASE_PATHS.registry}/tentaflow-${serviceId}:latest`,
      `    container_name: tentaflow-${serviceId}`,
      '    restart: unless-stopped',
      '    ports:',
      `      - "${port}:${internalPort}"`,
      `      - "${port}:${internalPort}/udp"`,
    ];

    if (hfToken) {
      yaml.push('    environment:');
      yaml.push(`      - HF_TOKEN=${hfToken}`);
    }

    yaml.push('    volumes:');
    yaml.push(...volumes);

    // Sekcja GPU tylko dla uslug z GPU
    if (hasGpu) {
      yaml.push(gpuSection(gpuId, 4));
    }

    yaml.push('    networks:');
    yaml.push(`      - ${BASE_PATHS.network}`);
    yaml.push(networksSection());

    return yaml.join('\n');
  }

  // Definicje standardowych uslug - kazda zwraca compose YAML
  const SERVICES = {
    tts: (params) => generateService('tts', params),
    stt: (params) => generateService('stt', params),
    embeddings: (params) => generateService('embeddings', params),
    rag: (params) => generateService('rag', params),
    tools: (params) => generateService('tools', params),
    memory: (params) => generateService('memory', params),
    reranker: (params) => generateService('reranker', params),
    'meeting-bot': (params) => generateMeetingBot(params),
    tentaflow: (params) => generateTentaFlow(params),
    bms: (params) => generateBms(params),
  };

  // Generator compose dla Meeting Bot (sidecar Teams)
  function generateMeetingBot(params) {
    const port = params.port || INTERNAL_PORTS['meeting-bot'];
    const cname = params.containerName || 'tentaflow-meeting-bot';
    const configDir = params.configDir || `${BASE_PATHS.config}/meeting-bot`;

    const meetingUrl = params.meetingUrl || '';
    const sttModel = params.sttModel || 'teams-stt';
    const ttsModel = params.ttsModel || 'teams-tts';
    const ttsVoice = params.ttsVoice || 'alloy';

    let yaml = [
      'services:',
      `  ${cname}:`,
      '    image: tentaflow-meeting-sidecar:dev',
      `    container_name: ${cname}`,
      '    restart: unless-stopped',
      '    ports:',
      `      - "${port}:${INTERNAL_PORTS['meeting-bot']}/udp"`,
      '    environment:',
      '      - RUST_LOG=info',
      `      - MEETING_URL=${meetingUrl}`,
      `      - AUTH_COOKIES_PATH=/tmp/cookies.json`,
      `      - QUIC_PORT=${INTERNAL_PORTS['meeting-bot']}`,
      `      - STT_MODEL=${sttModel}`,
      `      - TTS_MODEL=${ttsModel}`,
      `      - TTS_VOICE=${ttsVoice}`,
      '    networks:',
      `      - ${BASE_PATHS.network}`,
      networksSection(),
    ];

    return yaml.join('\n');
  }

  // Generator compose dla TentaFlow (NextApp)
  function generateTentaFlow(params) {
    const port = params.port || INTERNAL_PORTS.tentaflow;
    const includeDb = params.includeDb !== false;
    const dbPassword = params.dbPassword || 'changeme';

    let yaml = [
      'services:',
      '  tentaflow:',
      `    image: ${BASE_PATHS.registry}/tentaflow:latest`,
      '    container_name: tentaflow',
      '    restart: unless-stopped',
      '    ports:',
      `      - "${port}:${INTERNAL_PORTS.tentaflow}"`,
      '    volumes:',
      `      - ${BASE_PATHS.config}/tentaflow/data:/app/data`,
      `      - ${BASE_PATHS.certs}:/app/certs:ro`,
      '    networks:',
      `      - ${BASE_PATHS.network}`,
    ];

    if (includeDb) {
      yaml.push('');
      yaml.push('  tentaflow-db:');
      yaml.push('    image: postgres:latest');
      yaml.push('    container_name: tentaflow-db');
      yaml.push('    restart: unless-stopped');
      yaml.push('    environment:');
      yaml.push('      - POSTGRES_DB=nextapp');
      yaml.push('      - POSTGRES_USER=postgres');
      yaml.push(`      - POSTGRES_PASSWORD=${dbPassword}`);
      yaml.push('    volumes:');
      yaml.push(`      - ${BASE_PATHS.config}/tentaflow/pgdata:/var/lib/postgresql/data`);
      yaml.push('    networks:');
      yaml.push(`      - ${BASE_PATHS.network}`);
    }

    yaml.push(networksSection());
    return yaml.join('\n');
  }

  // Generator compose dla BMS (PAK.BMS)
  function generateBms(params) {
    const port = params.port || INTERNAL_PORTS.bms;
    const emisePort = params.emisePort || INTERNAL_PORTS.bms_emise;
    const includeDb = params.includeDb !== false;
    const dbPassword = params.dbPassword || 'changeme';

    let yaml = [
      'services:',
      '  tentaflow-bms:',
      `    image: ${BASE_PATHS.registry}/tentaflow-bms:latest`,
      '    container_name: tentaflow-bms',
      '    restart: unless-stopped',
      '    ports:',
      `      - "${port}:${INTERNAL_PORTS.bms}"`,
      `      - "${emisePort}:${INTERNAL_PORTS.bms_emise}"`,
    ];

    if (includeDb) {
      yaml.push('    environment:');
      yaml.push('      - CLICKHOUSE_HOST=tentaflow-bms-clickhouse');
      yaml.push('      - CLICKHOUSE_PORT=9000');
      yaml.push('      - CLICKHOUSE_DATABASE=pak_bms');
      yaml.push('      - CLICKHOUSE_USERNAME=default');
      yaml.push(`      - CLICKHOUSE_PASSWORD=${dbPassword}`);
    }

    yaml.push('    volumes:');
    yaml.push(`      - ${BASE_PATHS.config}/bms/data:/app/data`);
    yaml.push(`      - ${BASE_PATHS.certs}:/app/certs:ro`);
    yaml.push('    networks:');
    yaml.push(`      - ${BASE_PATHS.network}`);

    if (includeDb) {
      yaml.push('');
      yaml.push('  tentaflow-bms-clickhouse:');
      yaml.push('    image: clickhouse/clickhouse-server:latest');
      yaml.push('    container_name: tentaflow-bms-clickhouse');
      yaml.push('    restart: unless-stopped');
      yaml.push('    environment:');
      yaml.push('      - CLICKHOUSE_DB=pak_bms');
      yaml.push('      - CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT=1');
      yaml.push('    volumes:');
      yaml.push(`      - ${BASE_PATHS.config}/bms/clickhouse:/var/lib/clickhouse`);
      yaml.push('    networks:');
      yaml.push(`      - ${BASE_PATHS.network}`);
    }

    yaml.push(networksSection());
    return yaml.join('\n');
  }

  // Wspolny szablon compose dla silnikow GPU inference (SGLang, vLLM)
  function gpuInferenceTemplate(image, params) {
    const port = params.port || INTERNAL_PORTS.llm;
    const gpuId = params.gpuId || '0';
    const hfToken = params.hfToken || '';
    const modelId = params.modelId || '';
    const shmSize = params.shmSize || '16g';
    const gpuMemUtil = params.gpuMemoryUtilization || '0.9';
    const cname = params.containerName || 'tentaflow-llm';
    const dataDir = `${BASE_PATHS.config}/llm/${cname}`;

    let yaml = [
      'services:',
      `  ${cname}:`,
      `    image: ${image}`,
      `    container_name: ${cname}`,
      '    restart: unless-stopped',
      '    ports:',
      `      - "${port}:${INTERNAL_PORTS.llm}"`,
      `      - "${port}:${INTERNAL_PORTS.llm_quic}/udp"`,
      '    environment:',
      `      - HF_TOKEN=${hfToken}`,
      `      - MODEL_ID=${modelId}`,
      `      - GPU_MEMORY_UTILIZATION=${gpuMemUtil}`,
      '    volumes:',
      `      - ${dataDir}:/data`,
      `      - ${BASE_PATHS.certs}:/data/certs:ro`,
      `      - ${BASE_PATHS.models}:/app/models`,
      `    shm_size: '${shmSize}'`,
      gpuSection(gpuId, 4),
      '    networks:',
      `      - ${BASE_PATHS.network}`,
      networksSection(),
    ];

    return yaml.join('\n');
  }

  // Generator compose dla SGLang
  function sglang(params) {
    return gpuInferenceTemplate(`${BASE_PATHS.registry}/tentaflow-llm-sglang:latest`, params);
  }

  // Generator compose dla vLLM
  function vllm(params) {
    return gpuInferenceTemplate(`${BASE_PATHS.registry}/tentaflow-llm-vllm:latest`, params);
  }

  // Generator compose dla Ollama
  function ollama(params) {
    const port = params.port || INTERNAL_PORTS.llm;
    const gpuId = params.gpuId || '0';
    const shmSize = params.shmSize || '16g';
    const cname = params.containerName || 'tentaflow-llm';
    const dataDir = `${BASE_PATHS.config}/llm/${cname}`;

    let yaml = [
      'services:',
      `  ${cname}:`,
      `    image: ${BASE_PATHS.registry}/tentaflow-llm-ollama:latest`,
      `    container_name: ${cname}`,
      '    restart: unless-stopped',
      '    ports:',
      `      - "${port}:${INTERNAL_PORTS.llm}"`,
      `      - "${port}:${INTERNAL_PORTS.llm_quic}/udp"`,
      '    volumes:',
      `      - ${dataDir}:/data`,
      `      - ${BASE_PATHS.certs}:/data/certs:ro`,
      `      - ${BASE_PATHS.models}:/app/models`,
      `      - ${dataDir}/ollama:/root/.ollama`,
      `    shm_size: '${shmSize}'`,
      gpuSection(gpuId, 4),
      '    networks:',
      `      - ${BASE_PATHS.network}`,
      networksSection(),
    ];

    return yaml.join('\n');
  }

  // Generator compose dla LLama.cpp
  function llamacpp(params) {
    const port = params.port || INTERNAL_PORTS.llm;
    const gpuId = params.gpuId || '0';
    const ggufPath = params.ggufPath || '';
    const shmSize = params.shmSize || '16g';
    const cname = params.containerName || 'tentaflow-llm';
    const dataDir = `${BASE_PATHS.config}/llm/${cname}`;

    let yaml = [
      'services:',
      `  ${cname}:`,
      `    image: ${BASE_PATHS.registry}/tentaflow-llm-llamacpp:latest`,
      `    container_name: ${cname}`,
      '    restart: unless-stopped',
      '    ports:',
      `      - "${port}:${INTERNAL_PORTS.llm}"`,
      `      - "${port}:${INTERNAL_PORTS.llm_quic}/udp"`,
      '    environment:',
      `      - GGUF_PATH=${ggufPath}`,
      '      - N_GPU_LAYERS=99',
      '    volumes:',
      `      - ${dataDir}:/data`,
      `      - ${BASE_PATHS.certs}:/data/certs:ro`,
      `      - ${BASE_PATHS.models}:/app/models`,
      `    shm_size: '${shmSize}'`,
      gpuSection(gpuId, 4),
      '    networks:',
      `      - ${BASE_PATHS.network}`,
      networksSection(),
    ];

    return yaml.join('\n');
  }

  // Generator config dla MLX (natywny in-process — bez Docker, bez Python)
  function mlx(params) {
    const port = params.port || INTERNAL_PORTS.llm;
    const modelId = params.modelId || '';
    const cname = params.containerName || 'tentaflow-llm';

    // MLX dziala in-process przez mlx-rs (Rust Metal bindings)
    // Brak osobnego procesu — model ladowany przez InferenceManager
    return JSON.stringify({
      engine: 'mlx',
      deploy_mode: 'native',
      in_process: true,
      model_id: modelId,
      port: port,
      container_name: cname,
    }, null, 2);
  }

  // Generatory per silnik LLM
  const LLM_ENGINES = { sglang, vllm, ollama, llamacpp, mlx };

  // Generowanie compose dla standardowej uslugi po ID
  function generate(serviceId, params = {}) {
    const fn = SERVICES[serviceId];
    if (!fn) {
      throw new Error(`${I18n.t('common.unknown')} service: ${serviceId}`);
    }
    return fn(params);
  }

  // Generowanie compose dla LLM z wybranym silnikiem
  function generateLLM(engineId, params = {}) {
    const fn = LLM_ENGINES[engineId];
    if (!fn) {
      throw new Error(`${I18n.t('common.unknown')} LLM engine: ${engineId}`);
    }
    return fn(params);
  }

  return {
    generate,
    generateLLM,
    SERVICES: Object.keys(SERVICES),
    LLM_ENGINES: Object.keys(LLM_ENGINES),
  };
})();
