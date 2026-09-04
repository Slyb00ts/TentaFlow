// ===== File: _test-register.js — installs the root-path resolver before any test module loads =====
import { register } from 'node:module';

register('./_test-resolver.js', import.meta.url);
