// =============================================================================
// File: sdk-runtime/component-registry-completeness.test.js
// Description: Asserts that EVERY component tag from the v1 catalog has a
// registered renderer after `bootstrapSdkRuntime()`. This is the guard that
// would have caught the live "no renderer registered for tag 0x0105" (Split)
// failure before it hit production.
//
// The tag list is hardcoded from docs/ADDON_UI_COMPONENT_CATALOG_v1.md
// (all 151 `### 0x....` component sections, catalog_version = 1). Tags without
// a renderer must be listed in KNOWN_MISSING below — any other gap fails.
// =============================================================================

import './_dom-test-harness.js';

// component-renderer.js is imported (via bootstrap.js) before anything else
// renderer-related, matching the production addon-app entry order — its
// module body also registers SearchBox/ScrollContainer/Split directly.
import { bootstrapSdkRuntime } from './bootstrap.js';
import { lookupComponentRenderer } from './component-renderer.js';

// Full catalog tag → name map, transcribed 1:1 from the `### 0x....` section
// headings of docs/ADDON_UI_COMPONENT_CATALOG_v1.md.
const CATALOG_TAGS = new Map([
  // §2 Structured Molecules (0x0000–0x00FF)
  [0x0001, 'Header'],
  [0x0002, 'PageHeader'],
  [0x0003, 'EmptyState'],
  [0x0004, 'SectionHeader'],
  [0x0005, 'Toolbar'],
  [0x0006, 'AppShell'],
  [0x0007, 'LoginShell'],
  [0x0008, 'ErrorBoundary'],
  [0x0009, 'WelcomeHero'],
  [0x000A, 'StatGroup'],
  [0x000B, 'WizardShell'],
  [0x000C, 'Inspector'],
  // §3 Layout primitives (0x0100–0x01FF)
  [0x0101, 'Flex'],
  [0x0102, 'Grid'],
  [0x0103, 'Stack'],
  [0x0104, 'Cluster'],
  [0x0105, 'Split'],
  [0x0106, 'Card'],
  [0x0107, 'SectionCard'],
  [0x0108, 'Divider'],
  [0x0109, 'Spacer'],
  [0x010A, 'Sidebar'],
  [0x010B, 'Tabs'],
  [0x010C, 'NavTabs'],
  [0x010D, 'Collapsible'],
  [0x010E, 'Accordion'],
  [0x010F, 'Tooltip'],
  [0x0110, 'Breadcrumb'],
  [0x0111, 'Pagination'],
  [0x0112, 'ScrollContainer'],
  // §4 Data display (0x0200–0x02FF)
  [0x0201, 'Text'],
  [0x0202, 'Heading'],
  [0x0203, 'Paragraph'],
  [0x0204, 'RichText'],
  [0x0205, 'MonoBlock'],
  [0x0206, 'CodeBlock'],
  [0x0207, 'KeyValue'],
  [0x0208, 'StatCard'],
  [0x0209, 'Stat'],
  [0x020A, 'Badge'],
  [0x020B, 'Chip'],
  [0x020C, 'Tag'],
  [0x020D, 'Avatar'],
  [0x020E, 'AvatarGroup'],
  [0x020F, 'BulletList'],
  [0x0210, 'Timeline'],
  [0x0211, 'Table'],
  [0x0212, 'List'],
  [0x0213, 'Tree'],
  [0x0214, 'EmptyCell'],
  [0x0215, 'Sparkline'],
  [0x0216, 'LineChart'],
  [0x0217, 'BarChart'],
  [0x0218, 'AreaChart'],
  [0x0219, 'PieChart'],
  [0x021A, 'StackedBar'],
  [0x021B, 'Heatmap'],
  [0x021C, 'Gauge'],
  [0x021D, 'ProgressBar'],
  [0x021E, 'RatingDisplay'],
  [0x021F, 'Diff'],
  [0x0220, 'Markdown'],
  [0x0221, 'DataDefinitionList'],
  [0x0222, 'JsonViewer'],
  [0x0223, 'CalendarMonth'],
  [0x0224, 'Image'],
  [0x0225, 'VisuallyHidden'],
  [0x0226, 'LiveRegion'],
  // §5 Form (0x0300–0x03FF)
  [0x0301, 'Input'],
  [0x0302, 'Textarea'],
  [0x0303, 'Select'],
  [0x0304, 'MultiSelect'],
  [0x0305, 'Combobox'],
  [0x0306, 'Autocomplete'],
  [0x0307, 'SearchBox'],
  [0x0308, 'TagInput'],
  [0x0309, 'MentionInput'],
  [0x030A, 'Toggle'],
  [0x030B, 'Checkbox'],
  [0x030C, 'Radio'],
  [0x030D, 'RadioGroup'],
  [0x030E, 'RadioCardGroup'],
  [0x030F, 'Slider'],
  [0x0310, 'RangeSlider'],
  [0x0311, 'SliderRow'],
  [0x0312, 'NumericInput'],
  [0x0313, 'CurrencyInput'],
  [0x0314, 'DatePicker'],
  [0x0315, 'DateRangePicker'],
  [0x0316, 'TimePicker'],
  [0x0317, 'DateTimePicker'],
  [0x0318, 'FileInput'],
  [0x0319, 'ColorPicker'],
  [0x031A, 'FormField'],
  [0x031B, 'FormGroup'],
  [0x031C, 'FormSection'],
  [0x031D, 'Form'],
  // §6 Action (0x0400–0x04FF)
  [0x0401, 'Button'],
  [0x0402, 'IconButton'],
  [0x0403, 'ButtonGroup'],
  [0x0404, 'LinkButton'],
  [0x0405, 'Link'],
  [0x0406, 'MenuButton'],
  [0x0407, 'Menu'],
  [0x0408, 'ActionBar'],
  [0x0409, 'SegmentedControl'],
  [0x040A, 'FilterChips'],
  [0x040B, 'WizardFooter'],
  [0x040C, 'Fab'],
  // §7 Feedback (0x0500–0x05FF)
  [0x0501, 'Alert'],
  [0x0502, 'Banner'],
  [0x0503, 'Callout'],
  [0x0504, 'Toast'],
  [0x0505, 'Hint'],
  [0x0506, 'Skeleton'],
  [0x0507, 'Spinner'],
  [0x0508, 'LoadingBar'],
  [0x0509, 'Modal'],
  [0x050A, 'Drawer'],
  [0x050B, 'Popover'],
  [0x050C, 'Sheet'],
  [0x050D, 'GateScreen'],
  [0x050E, 'ConfirmationDialog'],
  [0x050F, 'OfflineBanner'],
  // §8 Specialized (0x0600–0x06FF)
  [0x0601, 'Canvas2D'],
  [0x0602, 'WebGLSurface'],
  [0x0603, 'WGPUSurface'],
  [0x0604, 'VideoStream'],
  [0x0605, 'LiveCameraTile'],
  [0x0606, 'MapView'],
  [0x0607, 'CodeEditor'],
  [0x0608, 'Terminal'],
  [0x0609, 'Audio'],
  [0x060A, 'IFrame'],
  [0x060B, 'ImageGallery'],
  [0x060C, 'Carousel'],
  [0x060D, 'PdfViewer'],
  [0x060E, 'FpsCounter'],
  [0x060F, 'StepProgress'],
  [0x0610, 'Stopwatch'],
  [0x0611, 'VirtualizedLog'],
  [0x0612, 'AudioCapture'],
  // §9 Domain-specific (0x0700–0x07FF)
  [0x0701, 'PermissionMatrix'],
  [0x0702, 'NetworkRuleEditor'],
  [0x0703, 'RelationGraph'],
  [0x0704, 'AlarmFeed'],
  [0x0705, 'WeeklyScheduleGrid'],
  [0x0706, 'AccessMatrix'],
  [0x0707, 'ReqCard'],
  [0x0708, 'DecisionRow'],
  [0x0709, 'Inbox'],
  [0x070A, 'RuntimeStatusGrid'],
]);

// Catalog tags with NO JS renderer yet. This list must ONLY shrink (remove a
// tag the moment its renderer ships) and must stay in sync with the
// RENDERER_NOT_IMPLEMENTED skip-list in
// tentaflow-core/addons/sdk-showcase/src/catalog.rs.
const KNOWN_MISSING = new Set([
  0x0601, // Canvas2D
  0x0602, // WebGLSurface
  0x0603, // WGPUSurface
  0x0701, // PermissionMatrix
  0x0702, // NetworkRuleEditor
  0x0703, // RelationGraph
  0x0704, // AlarmFeed
  0x0705, // WeeklyScheduleGrid
  0x0706, // AccessMatrix
  0x0707, // ReqCard
  0x0708, // DecisionRow
  0x0709, // Inbox
  0x070A, // RuntimeStatusGrid
]);

bootstrapSdkRuntime();

const fmt = (tag, name) =>
  `0x${tag.toString(16).padStart(4, '0').toUpperCase()} ${name}`;

const missing = [];
const stale = [];
for (const [tag, name] of CATALOG_TAGS) {
  const hasRenderer = typeof lookupComponentRenderer(tag) === 'function';
  if (KNOWN_MISSING.has(tag)) {
    // A renderer appearing here means someone implemented the tag without
    // removing it from KNOWN_MISSING (and the catalog.rs skip-list).
    if (hasRenderer) stale.push(fmt(tag, name));
  } else if (!hasRenderer) {
    missing.push(fmt(tag, name));
  }
}

const total = CATALOG_TAGS.size;
const registered = total - KNOWN_MISSING.size + stale.length - missing.length;
// eslint-disable-next-line no-console
console.log(
  `registry completeness: ${registered}/${total} catalog tags have a renderer ` +
  `(${KNOWN_MISSING.size} known missing)`
);
if (missing.length > 0) {
  // eslint-disable-next-line no-console
  console.log(`MISSING renderers (${missing.length}):\n  ${missing.join('\n  ')}`);
}
if (stale.length > 0) {
  // eslint-disable-next-line no-console
  console.log(
    `STALE KNOWN_MISSING entries — renderer exists, remove from this list AND ` +
    `from catalog.rs RENDERER_NOT_IMPLEMENTED (${stale.length}):\n  ${stale.join('\n  ')}`
  );
}

if (typeof process !== 'undefined' && (missing.length > 0 || stale.length > 0)) {
  process.exit(1);
}

export { missing, stale };
