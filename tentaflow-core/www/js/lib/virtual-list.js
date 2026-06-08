// =============================================================================
// Plik: lib/virtual-list.js
// Opis: Vanilla virtualizer pionowej listy. Cechy:
//       - Dynamic item heights via getItemHeight(index, item) callback
//         (cache + invalidation gdy resize)
//       - Overscan (default 5) — pre-render poza viewport dla plynnego scroll
//       - Total height = suma wszystkich itemSize (cumulative offsets)
//       - Scroll-to-bottom auto-pin (chat use case)
//       - ResizeObserver na container — invalidate cache + remeasure
//       - rAF-throttled scroll handler
//
//       Przyklad:
//         const list = createVirtualList(hostEl, {
//           items,
//           getItemHeight: (i, item) => measureItemHeight(item.text, {maxWidth}),
//           renderItem: (i, item) => `<div class="msg">${item.text}</div>`,
//           overscan: 5,
//           pinToBottom: true,
//         });
//         list.setItems(newItems);
//         list.append(newItem);
//         list.scrollToBottom();
//         list.destroy();
// =============================================================================

const DEFAULT_OVERSCAN = 5;

/// Tworzy wirtualizowana liste. Zwraca handle z metodami sterujacymi.
export function createVirtualList(host, opts) {
  if (!host) throw new Error('virtual-list: host is required');
  const overscan = opts.overscan ?? DEFAULT_OVERSCAN;
  let items = opts.items ?? [];
  const getItemHeight = opts.getItemHeight;
  const renderItem = opts.renderItem;
  const pinToBottom = opts.pinToBottom ?? false;
  const onScroll = opts.onScroll;
  // Pomiar realnych wysokości (offsetHeight) + anchoring + iteracyjny
  // scrollToBottom jest OPT-IN. Domyślnie WYŁĄCZONY: konsument używa czystych
  // estymat z getItemHeight i prostego scrollToBottom. Czat włącza go (treść
  // markdown/think rozjeżdża się z estymatą), meeting.js zostaje na estymatach,
  // bo padding pierwszego/ostatniego renderowanego itemu zależy od granicy
  // wycinka i mierzenie outer offsetHeight cache'owałoby przejściowy padding.
  const measureHeights = opts.measureHeights ?? false;

  if (typeof getItemHeight !== 'function') throw new Error('virtual-list: getItemHeight required');
  if (typeof renderItem !== 'function') throw new Error('virtual-list: renderItem required');

  // Setup DOM: scroll container + spacer (full height) + viewport (absolute positioned items)
  host.classList.add('vlist-host');
  host.innerHTML = `
    <div class="vlist-spacer" style="position:relative;width:100%;">
      <div class="vlist-viewport" style="position:absolute;top:0;left:0;right:0;"></div>
    </div>
  `;
  const spacer = host.querySelector('.vlist-spacer');
  const viewport = host.querySelector('.vlist-viewport');

  // Cache: index → height. Invalidated przy setItems/resize.
  let heightCache = new Float64Array(items.length);
  let offsetCache = new Float64Array(items.length + 1);
  let totalHeight = 0;
  let containerWidth = host.clientWidth || 0;

  // Pinning state
  let pinned = pinToBottom;
  let lastRenderRange = { start: -1, end: -1 };

  // rAF throttle
  let rafId = null;

  // Osobna flaga rAF dla koalescencji pomiaru+renderu ogona w streamingu
  // (gałąź existing, measureHeights). updateTail bywa wołane co token
  // (100-250 tok/s); tani patch treści robimy synchronicznie co token, ale
  // drogi odczyt offsetHeight + scroll + render planujemy raz na klatkę.
  // Mechanizm jest niezależny od rafId (scroll/render) — własna flaga, by
  // tail-rAF nie kolidował ze scroll-rAF i nie planował kaskady renderów.
  let tailMeasureRaf = null;

  // rAF łańcucha zbieżności scrollToBottom (gałąź measureHeights). Trzymamy id
  // ostatnio zaplanowanego `step`, by destroy() mógł go anulować — czat reużywa
  // ten sam host element, a osierocony step trzymałby referencję do hosta i
  // wymuszał scrollTop na NOWEJ liście po remount.
  let scrollBottomRaf = null;

  // Flaga anty-loop: programowa zmiana host.scrollTop (anchoring / pin) odpala
  // listener 'scroll'. Bez tego scroll-kompensacja byłaby traktowana jak
  // user-scroll i przeliczałaby pinned/render, co prowadzi do skoków i pętli.
  let adjustingScroll = false;

  // scrollToBottom iteruje przez rAF do zbieżności; każdy przebieg dociąga
  // widok na dno i re-pinuje. Gdy user przewinie w GÓRĘ między zaplanowanymi
  // klatkami, kolejny przebieg ściągnąłby go z powrotem na dno i przejął
  // kontrolę — nie dałoby się czytać podczas długiej zbieżności. Flaga
  // scrollToBottomActive mówi że pętla trwa; userScrolledDuringConverge zapala
  // się przy PRAWDZIWYM user-scrollu (gałąź poza adjustingScroll), co przerywa
  // pętlę i zostawia usera tam gdzie przewinął.
  let scrollToBottomActive = false;
  let userScrolledDuringConverge = false;
  // Licznik generacji pętli zbieżności. scrollToBottom() bywa wołane ponownie
  // (np. append() user-msg, zaraz potem append() assistant-msg) zanim poprzednia
  // pętla rAF się skończy. Stara i nowa pętla współdzielą scrollToBottomActive /
  // userScrolledDuringConverge, więc bez tokena starsza pętla po dojściu do dna
  // zerowała scrollToBottomActive (gasząc detekcję user-scrolla dla nowszej), a
  // jej zakolejkowany rAF ściągał czat z powrotem na dno. Każde wywołanie bumpuje
  // ten licznik; każdy step sprawdza swój domknięty myGen i jeśli jest
  // przestarzały — cicho wychodzi bez dotykania wspólnych flag (należą do nowszej).
  let scrollToBottomGen = 0;

  // Programowy zapis scrollTop z ustawieniem flagi anty-loop TYLKO gdy wartość
  // realnie się zmieni. Klampujemy do prawidłowego zakresu PRZED porównaniem,
  // bo przeglądarka i tak ogranicza scrollTop do [0, scrollHeight - clientHeight].
  // Porównanie z realną docelową wartością (po klampie) gwarantuje, że gdy
  // przypisanie byłoby no-opem (np. już na dnie), nie uzbroimy flagi — inaczej
  // brak zdarzenia 'scroll' zostawiłby flagę wiszącą i połknął kolejny prawdziwy
  // user-scroll.
  function setScrollTop(value) {
    const maxTop = Math.max(0, host.scrollHeight - host.clientHeight);
    const clamped = Math.max(0, Math.min(value, maxTop));
    if (Math.abs(host.scrollTop - clamped) > 0.5) {
      adjustingScroll = true;
      host.scrollTop = clamped;
    }
  }

  function recompute() {
    const len = items.length;
    if (heightCache.length !== len) heightCache = new Float64Array(len);
    if (offsetCache.length !== len + 1) offsetCache = new Float64Array(len + 1);
    let acc = 0;
    for (let i = 0; i < len; i++) {
      const h = getItemHeight(i, items[i]);
      heightCache[i] = h;
      offsetCache[i] = acc;
      acc += h;
    }
    offsetCache[len] = acc;
    totalHeight = acc;
    spacer.style.height = `${totalHeight}px`;
  }

  // Realokuje heightCache (do n) i offsetCache (do n+1), zachowując dotychczasowe
  // wartości (Float64Array.set kopiuje istniejące). Używane przy inkrementalnym
  // append: pozwala dorzucić nowy item BEZ pełnego recompute, który nadpisałby
  // już ZMIERZONE realne wysokości estymatami z getItemHeight.
  function growCachesTo(n) {
    if (heightCache.length < n) {
      const nh = new Float64Array(n);
      nh.set(heightCache);
      heightCache = nh;
    }
    if (offsetCache.length < n + 1) {
      const no = new Float64Array(n + 1);
      no.set(offsetCache);
      offsetCache = no;
    }
  }

  // Przelicza offsetCache od indeksu `from` w górę (do końca) na podstawie
  // aktualnego heightCache. Używane po pomiarze realnych wysokości — nie ma
  // sensu liczyć od zera, bo offsety poniżej `from` się nie zmieniły.
  function recomputeOffsetsFrom(from) {
    const len = items.length;
    let acc = offsetCache[from];
    for (let i = from; i < len; i++) {
      offsetCache[i] = acc;
      acc += heightCache[i];
    }
    offsetCache[len] = acc;
    totalHeight = acc;
    spacer.style.height = `${totalHeight}px`;
  }

  // Mierzy realne wysokości właśnie wyrenderowanych itemów (offsetHeight) i
  // koryguje heightCache. getItemHeight daje tylko ESTYMATĘ — realna treść
  // (markdown, kod, zwijane <think>, avatar/meta) bywa wyższa/niższa, przez co
  // matematyka scrolla liczona z estymat nie trafia w realne dno i widok skacze.
  //
  // Po korekcie stosujemy scroll-anchoring: trzymamy wizualnie nieruchomy
  // pierwszy WIDOCZNY wiersz — ten przecinający górną krawędź viewportu, NIE
  // pierwszy renderowany startIdx. Przy overscan>0 startIdx leży ~overscan
  // wierszy NAD viewportem; offsetCache[startIdx] zwykle się nie zmienia (bo
  // minChanged>=startIdx), więc kotwiczenie do startIdx dawałoby delta=0 i
  // ZERO kompensacji, mimo że wiersze overscan między startIdx a krawędzią
  // viewportu mogą zmienić wysokość — wtedy offset pierwszego widocznego się
  // przesuwa i widoczna treść jedzie. Kotwicząc do pierwszego widocznego
  // (anchorIdx) kompensujemy zmiany wysokości WSZYSTKICH wierszy nad nim
  // (łącznie z overscan nad viewportem), trzymając go nieruchomo.
  //
  // Zwraca true jeśli cokolwiek się zmieniło (przydatne dla konwergencji pinu).
  function measureRendered(startIdx) {
    const els = viewport.querySelectorAll('.vlist-item[data-vidx]');
    if (els.length === 0) return false;

    let minChanged = -1;
    for (const el of els) {
      const i = +el.dataset.vidx;
      // render() ustawia min-height = stary heightCache[i], więc offsetHeight nigdy
      // nie zejdzie poniżej poprzedniej (estymowanej/zmierzonej) wartości. Gdy treść
      // się KURCZY (np. <think> zwija się po końcu streamingu) zmierzylibyśmy zawyżoną
      // wysokość → totalHeight/spacer zostają za duże i na dole robi się pusta dziura.
      // Zerujemy min-height, by offsetHeight oddał realną wysokość treści (też mniejszą).
      el.style.minHeight = '0px';
      const real = el.offsetHeight;
      if (Math.abs(real - heightCache[i]) > 0.5) {
        heightCache[i] = real;
        if (minChanged < 0 || i < minChanged) minChanged = i;
      }
    }
    if (minChanged < 0) return false;

    // Anchor = pierwszy widoczny wiersz. anchorIdx i prevAnchorOffset liczone
    // PRZED recompute na STARYCH offsetach (spójnych z prevScrollTop);
    // newAnchorOffset PO recompute na nowych — różnica to dryf nad kotwicą.
    const prevScrollTop = host.scrollTop;
    const anchorIdx = findStartIndex(prevScrollTop);
    const prevAnchorOffset = offsetCache[anchorIdx];

    recomputeOffsetsFrom(minChanged);

    // transform ustawiamy wg startIdx (placement renderowanego bloku) i PO
    // recompute, z aktualnego offsetCache — spójny ze scrollTop liczonym niżej,
    // bo oba czytają ten sam świeży offsetCache.
    viewport.style.transform = `translateY(${offsetCache[startIdx]}px)`;

    const newAnchorOffset = offsetCache[anchorIdx];
    const delta = newAnchorOffset - prevAnchorOffset;
    if (delta !== 0) setScrollTop(prevScrollTop + delta);
    return true;
  }

  // Binary search: znajdz pierwszy index ktory offset >= scrollTop
  function findStartIndex(scrollTop) {
    let lo = 0;
    let hi = items.length - 1;
    if (hi < 0) return 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const offset = offsetCache[mid];
      if (offset === scrollTop) return mid;
      if (offset < scrollTop) lo = mid + 1;
      else hi = mid - 1;
    }
    return Math.max(0, hi);
  }

  function render() {
    if (items.length === 0) {
      viewport.innerHTML = '';
      lastRenderRange = { start: 0, end: 0 };
      return;
    }
    const scrollTop = host.scrollTop;
    const viewportH = host.clientHeight;
    const startIdx = Math.max(0, findStartIndex(scrollTop) - overscan);
    let endIdx = startIdx;
    let acc = 0;
    while (endIdx < items.length && acc < viewportH + overscan * 2 * 50) {
      acc += heightCache[endIdx];
      endIdx += 1;
    }
    endIdx = Math.min(items.length, endIdx + overscan);

    // Skip if same range (avoid reflow)
    if (startIdx === lastRenderRange.start && endIdx === lastRenderRange.end) return;
    lastRenderRange = { start: startIdx, end: endIdx };

    const offsetTop = offsetCache[startIdx];
    viewport.style.transform = `translateY(${offsetTop}px)`;

    // Render — buduj string, jednorazowy innerHTML write
    const parts = [];
    for (let i = startIdx; i < endIdx; i++) {
      parts.push(`<div class="vlist-item" data-vidx="${i}" style="min-height:${heightCache[i]}px;">${renderItem(i, items[i])}</div>`);
    }
    viewport.innerHTML = parts.join('');

    // Pomiar realnych wysokości + anchoring jest OPT-IN. Bez measureHeights
    // render kończy się tu jak w oryginale — czyste estymaty, bez pomiaru i bez
    // planowania kolejnego renderu.
    if (!measureHeights) return;

    // Pomiar realnych wysokości + anchoring. NIE wołamy render() rekurencyjnie —
    // pomiar koryguje cache i scrollTop, a kolejny scroll/rAF dokończy render
    // jeśli zakres się zmienił (chroni przed pętlą render→measure→render).
    const changed = measureRendered(startIdx);

    // Gdy pomiar SKURCZYŁ wysokości (np. getItemHeight przeszacował zwinięty
    // <think> / krótkie wiersze), endIdx powyżej został policzony na starych
    // zawyżonych estymatach — viewport mógł zostać niedopełniony i na dole DOM
    // robi się pusta dziura DOPÓKI user nie zascrolluje. Planujemy JEDEN kolejny
    // render przez rAF, by dobrać brakujące wiersze na świeżych wysokościach.
    // Konwergencja: przerenderowujemy TYLKO gdy pomiar coś zmienił — gdy
    // wysokości są już dokładne, measureRendered zwróci false → brak kolejnego
    // renderu → STOP (zbiega w 1-2 przebiegach, brak pętli render→measure→render).
    // Guard rafId==null: nie nadpisujemy rAF zajętego przez onScrollHandler.
    if (changed && rafId == null) {
      // Wymuszamy render mimo guardu „skip if same range": zakres mógł się
      // zmienić, ale resetujemy lastRenderRange dla pewności domknięcia.
      lastRenderRange = { start: -1, end: -1 };
      rafId = requestAnimationFrame(() => {
        rafId = null;
        render();
      });
    }
  }

  function onScrollHandler() {
    // Programowa korekta scrollTop (anchoring / pin) odpala ten listener —
    // resetujemy flagę i wychodzimy bez przeliczania pinned/render, żeby nie
    // traktować kompensacji jak user-scroll.
    if (adjustingScroll) {
      adjustingScroll = false;
      return;
    }
    // Tu jesteśmy tylko przy PRAWDZIWYM user-scrollu (programowy setScrollTop
    // wpada w gałąź adjustingScroll powyżej). Jeśli trwa zbieżność
    // scrollToBottom, user właśnie chce przejąć kontrolę — sygnalizujemy to,
    // by następny `step` przerwał pętlę zamiast ściągać go na dno.
    if (scrollToBottomActive) userScrolledDuringConverge = true;
    // Recompute pinned state: <30px from bottom counts as "still pinned".
    const scrollTop = host.scrollTop;
    const distanceFromBottom = totalHeight - (scrollTop + host.clientHeight);
    const nextPinned = distanceFromBottom < 30;
    const changed = nextPinned !== pinned;
    pinned = nextPinned;
    if (rafId == null) {
      rafId = requestAnimationFrame(() => {
        rafId = null;
        render();
        onScroll?.(scrollTop, distanceFromBottom, { pinned, changed });
      });
    }
  }

  // ResizeObserver — gdy szerokosc kontenera sie zmienia, wszystkie wysokosci
  // moga sie zmienic (text wrapping). Recompute + render.
  const resizeObserver = new ResizeObserver((entries) => {
    const newWidth = entries[0].contentRect.width;
    if (Math.abs(newWidth - containerWidth) > 1) {
      containerWidth = newWidth;
      recompute();
      if (pinned) scrollToBottom();
      else render();
    }
  });
  resizeObserver.observe(host);

  host.addEventListener('scroll', onScrollHandler, { passive: true });

  // Initial render
  recompute();
  if (pinToBottom) scrollToBottom();
  else render();

  function setItems(next) {
    const wasPinned = pinned;
    items = next ?? [];
    recompute();
    if (wasPinned) scrollToBottom();
    else render();
  }

  // Dokłada JEDEN item na koniec rozszerzając cache INKREMENTALNIE — bez pełnego
  // recompute, które przeliczyłoby cały heightCache z estymat getItemHeight,
  // gubiąc realne wysokości zmierzone wcześniej dla wierszy nad viewportem. Nowy
  // item dostaje estymatę (zmierzy się po renderze), istniejące wartości zostają.
  // offsetCache[i] = offset POCZĄTKU itemu i, offsetCache[len] = total.
  function appendOne(item) {
    const newIdx = items.length;
    items.push(item);
    const len = items.length;
    growCachesTo(len);
    const h = getItemHeight(newIdx, item);
    heightCache[newIdx] = h;
    offsetCache[newIdx] = newIdx > 0 ? offsetCache[newIdx - 1] + heightCache[newIdx - 1] : 0;
    offsetCache[len] = offsetCache[newIdx] + h;
    totalHeight = offsetCache[len];
    spacer.style.height = `${totalHeight}px`;
  }

  function append(item) {
    const wasPinned = pinned;
    appendOne(item);
    if (wasPinned) scrollToBottom();
    else render();
  }

  function appendBatch(newItems) {
    if (!newItems?.length) return;
    const wasPinned = pinned;
    for (const item of newItems) appendOne(item);
    if (wasPinned) scrollToBottom();
    else render();
  }

  // Iteracyjny pin do REALNEGO dna. Po pomiarze realnych wysokości totalHeight
  // rośnie/maleje, więc jednorazowy skok host.scrollTop = scrollHeight (liczony
  // na estymatach) nie trafia w dno i najnowsza wiadomość bywa ucięta. Pętla:
  // ustaw scrollTop=scrollHeight → render (z pomiarem) → jeśli totalHeight albo
  // pozycja dna się zmieniły, powtórz. Iterujemy do ZBIEŻNOŚCI, nie do stałej
  // liczby przebiegów: przy długiej liście wiele przeszacowanych okien po kolei
  // koryguje wysokości, a każda korekta kurczy totalHeight i odsłania WCZEŚNIEJSZE,
  // jeszcze niezmierzone okno — sztywny limit 3 zostawiał viewport powyżej dna,
  // a programowy setScrollTop nie odpala onScrollHandler→render, więc ostatnie
  // okno nie było renderowane. Każde okno mierzymy raz, więc liczba przebiegów
  // jest skończona; SAFETY_CAP to tylko bezpiecznik anty-loop na subpikselowe
  // oscylacje (realnie zbiega w 2-4 przebiegach).
  function scrollToBottom() {
    // Bez pomiaru wysokości totalHeight/scrollHeight liczone są z estymat i są
    // stabilne — wystarczy oryginalne proste zachowanie: jedno przejście rAF,
    // skok na dno, pin, render. Bez pętli zbieżności i tokenów generacji.
    if (!measureHeights) {
      requestAnimationFrame(() => {
        host.scrollTop = host.scrollHeight;
        pinned = true;
        render();
      });
      return;
    }
    const SAFETY_CAP = 20;
    let iterations = 0;
    // Unieważnij wszystkie wcześniejsze pętle: ich step-y zobaczą, że
    // myGen !== scrollToBottomGen i wygasną. Od tej chwili tylko ta pętla jest
    // właścicielem wspólnych flag.
    const myGen = ++scrollToBottomGen;
    scrollToBottomActive = true;
    userScrolledDuringConverge = false;
    const step = () => {
      scrollBottomRaf = null;
      // Przestarzała pętla: nowsze scrollToBottom() przejęło kontrolę. Wychodzimy
      // cicho — NIE zerujemy wspólnych flag, bo należą teraz do nowszej generacji.
      if (myGen !== scrollToBottomGen) return;
      // User przewinął w górę w trakcie zbieżności — przerywamy pętlę i NIE
      // domykamy na dno. onScrollHandler ustawił już pinned=false na podstawie
      // jego pozycji, więc zostaje tam gdzie przewinął. Obsługuje to tylko
      // najnowsza generacja (starsze wyszły wyżej na checku generacji).
      if (userScrolledDuringConverge) {
        scrollToBottomActive = false;
        return;
      }
      const prevTotal = totalHeight;
      setScrollTop(host.scrollHeight);
      pinned = true;
      lastRenderRange = { start: -1, end: -1 };
      render();
      iterations += 1;

      // STOP, gdy stan jest stabilny: wysokość listy się nie zmieniła ORAZ
      // faktycznie dotarliśmy do dna (scrollTop na maxTop). maxTop liczymy po
      // renderze, bo render() mógł skorygować scrollHeight przez pomiar.
      const maxTop = Math.max(0, host.scrollHeight - host.clientHeight);
      const heightStable = Math.abs(totalHeight - prevTotal) <= 0.5;
      const atBottom = host.scrollTop >= maxTop - 0.5;
      const converged = heightStable && atBottom;

      if (!converged && iterations < SAFETY_CAP) {
        scrollBottomRaf = requestAnimationFrame(step);
        return;
      }

      if (!converged) {
        // Bezpiecznik zadziałał — coś oscyluje subpikselowo. Ostrzegamy raz.
        console.warn('virtual-list: scrollToBottom nie zbiegło w', SAFETY_CAP, 'przebiegach');
      }

      // Domknięcie na ustabilizowanym spacerze: programowy scroll nie wywołuje
      // onScrollHandler→render, więc wymuszamy render po finalnym setScrollTop,
      // aby najnowsza wiadomość była na pewno wyrenderowana i widoczna na dnie.
      setScrollTop(host.scrollHeight);
      pinned = true;
      lastRenderRange = { start: -1, end: -1 };
      render();
      // Zeruj wspólną flagę tylko jeśli wciąż jesteśmy najnowszą generacją —
      // w trakcie tego stepu mogło wystartować nowsze scrollToBottom().
      if (myGen === scrollToBottomGen) scrollToBottomActive = false;
    };
    scrollBottomRaf = requestAnimationFrame(step);
  }

  // Incremental tail update: only the last item changed (streaming case).
  // Patchuje `innerHTML` istniejacego `.vlist-item[data-vidx=lastIdx]` zamiast
  // przepisywac caly viewport.innerHTML — przy streaming LLM (100-250 tok/s)
  // render calego viewport co klatke powodowal skoki UI. Pelny render robimy
  // tylko gdy item spadl poza widoczny zakres (musi pojawic sie od nowa).
  function updateTail() {
    const len = items.length;
    if (len === 0) return;
    const lastIdx = len - 1;
    const prevH = heightCache[lastIdx] || 0;

    const existing = viewport.querySelector(`[data-vidx="${lastIdx}"]`);
    if (existing) {
      // Patchuj tylko ostatni bubble. Reszta viewport DOM nietknieta,
      // brak scroll jank / focus loss / reflow reszty.
      existing.innerHTML = renderItem(lastIdx, items[lastIdx]);
      if (measureHeights) {
        // Tani patch treści zrobiliśmy synchronicznie powyżej (co token). Drogi
        // odczyt offsetHeight wymusza layout — robienie go co token przy 100-250
        // tok/s daje per-token layout thrash i jank na main-thread. Dlatego pomiar
        // realnej wysokości, korektę cache, pin-scroll i ewentualny re-render
        // KOALESCUJEMY do JEDNEJ klatki rAF: niezależnie ile tokenów wpadnie między
        // klatkami, layout czytamy najwyżej raz. Planujemy tylko gdy tail-rAF nie
        // jest jeszcze zaplanowany (guard anty-loop); callback zeruje flagę.
        if (tailMeasureRaf == null) {
          tailMeasureRaf = requestAnimationFrame(() => {
            tailMeasureRaf = null;
            // Stan mógł się zmienić między zaplanowaniem a wykonaniem klatki
            // (kolejne tokeny, setItems, przebudowa listy) — przeliczamy aktualny
            // ogon zamiast domykać na nieaktualnym lastIdx/prevH.
            const curLen = items.length;
            if (curLen === 0) return;
            const curIdx = curLen - 1;
            // el z gałęzi synchronicznej mógł wypaść z zakresu / lista mogła się
            // przebudować — pobieramy świeży węzeł. Brak węzła → nie czytamy
            // martwego DOM, tylko wymuszamy render dolnego okna (po pinie).
            const el = viewport.querySelector(`[data-vidx="${curIdx}"]`);
            if (!el) {
              if (pinned) setScrollTop(host.scrollHeight);
              lastRenderRange = { start: -1, end: -1 };
              render();
              return;
            }
            // Zerujemy min-height, by offsetHeight oddał też SKURCZENIE treści
            // (koniec streamingu, zwinięty <think>) — inaczej min-height = stara
            // wysokość trzymałby zawyżony pomiar i spacer zostałby za duży.
            el.style.minHeight = '0px';
            const realH = el.offsetHeight;
            el.style.minHeight = `${realH}px`;
            if (Math.abs(realH - heightCache[curIdx]) > 0.5) {
              heightCache[curIdx] = realH;
              totalHeight = offsetCache[curIdx] + realH;
              offsetCache[curLen] = totalHeight;
              spacer.style.height = `${totalHeight}px`;
            }
            // Pin: skacz na dno, a następnie wymuś render dolnego okna W TEJ SAMEJ
            // klatce. setScrollTop oznacza zdarzenie scroll jako programowe, więc
            // onScrollHandler je zignoruje i sam z siebie nie przeliczy zakresu —
            // bez tego renderu skurczenie ogona zostawiałoby pustą dziurę na górze
            // dolnego viewportu do następnego user-scrolla. Render PO setScrollTop,
            // by zakres policzył się na dolnym scrollTop. Render w tym callbacku
            // korzysta z osobnego rafId (scroll/render) — nie planuje kolejnego
            // tail-rAF, więc nie powstaje pętla.
            if (pinned) {
              setScrollTop(host.scrollHeight);
              lastRenderRange = { start: -1, end: -1 };
              render();
            }
          });
        }
      } else {
        // Bez pomiaru (meeting): aktualizuj wysokość z estymaty getItemHeight jak
        // w oryginale — bez rAF, bez odczytu layoutu.
        const estH = getItemHeight(lastIdx, items[lastIdx]);
        if (estH !== prevH) {
          heightCache[lastIdx] = estH;
          totalHeight = offsetCache[lastIdx] + estH;
          offsetCache[len] = totalHeight;
          spacer.style.height = `${totalHeight}px`;
        }
        if (pinned) setScrollTop(host.scrollHeight);
      }
      return;
    }

    // Item poza widocznym zakresem — pelny render (pomiar w render() skoryguje
    // realną wysokość, gdy item wejdzie w viewport).
    const estH = getItemHeight(lastIdx, items[lastIdx]);
    if (estH !== prevH) {
      heightCache[lastIdx] = estH;
      totalHeight = offsetCache[lastIdx] + estH;
      offsetCache[len] = totalHeight;
      spacer.style.height = `${totalHeight}px`;
    }
    // Gdy przypięte do dna: najpierw skacz na dno, DOPIERO potem renderuj.
    // setScrollTop oznacza następne zdarzenie scroll jako programowe, więc
    // onScrollHandler je zignoruje — gdyby render() poszedł przed skokiem,
    // policzyłby zakres na starym (górnym) scrollTop i ogon zostałby pusty,
    // dopóki user sam nie zascrolluje. Render po skoku liczy zakres na nowym
    // (dolnym) scrollTop i od razu rysuje dolne okno z ogonem.
    if (pinned) setScrollTop(host.scrollHeight);
    lastRenderRange = { start: -1, end: -1 };
    render();
  }

  function scrollToIndex(idx) {
    if (idx < 0 || idx >= items.length) return;
    setScrollTop(offsetCache[idx]);
    lastRenderRange = { start: -1, end: -1 };
    render();
  }

  function destroy() {
    resizeObserver.disconnect();
    host.removeEventListener('scroll', onScrollHandler);
    if (rafId != null) cancelAnimationFrame(rafId);
    if (tailMeasureRaf != null) cancelAnimationFrame(tailMeasureRaf);
    if (scrollBottomRaf != null) cancelAnimationFrame(scrollBottomRaf);
    host.classList.remove('vlist-host');
    host.innerHTML = '';
  }

  return {
    setItems,
    append,
    appendBatch,
    updateTail,
    scrollToBottom,
    scrollToIndex,
    destroy,
    refresh: () => {
      recompute();
      if (pinned) scrollToBottom();
      else render();
    },
    get items() { return items; },
    get pinned() { return pinned; },
  };
}
