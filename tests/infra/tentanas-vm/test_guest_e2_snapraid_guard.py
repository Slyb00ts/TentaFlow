# =============================================================================
# Plik: test_guest_e2_snapraid_guard.py
# Opis: Walidacja pinów i montowań przez rzeczywisty guard oraz odczyt fixture.
# Przykład: python3 -B -m unittest discover -s tests/infra/tentanas-vm -p test_guest_e2_snapraid_guard.py
# =============================================================================

import contextlib
import copy
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import types
import unittest
from unittest import mock

import guest_e2_snapraid as harness


def pinned_audit():
    source = Path(__file__).with_name('readonly-elastic-audit.py').read_bytes()
    if hashlib.sha256(source).hexdigest() != harness.PINS['readonly-elastic-audit.py']:
        raise ValueError('Niezgodny przypięty audit używany przez test')
    module = types.ModuleType('readonly_elastic_audit_test')
    exec(compile(source, 'readonly-elastic-audit.py', 'exec'), module.__dict__)
    return module


class GuardInputs:
    def __init__(self):
        audit = pinned_audit()
        self.ROOT = audit.ROOT
        self.decode_json, self.validate_journal = audit.decode_json, audit.validate_journal
        self.inventory, self.parse_blkid = audit.inventory, audit.parse_blkid
        self.mount_rows, self.config_directives = audit.mount_rows, audit.config_directives
        self.expected_paths = audit.expected_paths
        _, self.roles, self.config, self.content, self.parity = audit.expected_paths(harness.CASE)
        self.targets = [role[3] for role in self.roles]
        self.spec = {'array_id': harness.ARRAY_ID, 'operation_id': harness.OPERATION_ID,
                     'owner': {'org_id': 'fixture-org', 'addon_id': 'fixture-addon'},
                     'name': harness.CASE, 'filesystem': 'xfs',
                     'data': [], 'parity': []}
        self.whole = [{'name': '/dev/vda', 'serial': 'tn-16e0a47bf6-os', 'maj:min': '8:0', 'parent': None}]
        self.mounts = []
        self.probes = {}
        self.stats = {'/': types.SimpleNamespace(st_dev=os.makedev(8, 1))}
        for index, (role, _, _, target) in enumerate(self.roles):
            name, minor = '/dev/vd' + chr(ord('b') + index), 16 * (index + 1)
            serial = 'tn-16e0a47bf6-' + ['data1', 'parity', 'spare'][index]
            wanted = {'disk_id': serial, 'serial': serial, 'bytes': audit.DISKS[serial],
                      'wwn': None, 'expected_uuid': harness.FS_UUIDS[index]}
            self.spec[role].append(wanted)
            self.whole.append({'name': name, 'serial': serial, 'wwn': None, 'maj:min': f'8:{minor}', 'parent': None})
            self.stats[name] = types.SimpleNamespace(st_mode=stat.S_IFBLK, st_rdev=os.makedev(8, minor))
            self.stats[target] = types.SimpleNamespace(st_dev=os.makedev(8, minor))
            self.probes[name] = {'UUID': wanted['expected_uuid'], 'TYPE': 'xfs'}
            self.mounts.append({'target': target, 'source': name, 'major_minor': f'8:{minor}',
                                'filesystem': 'xfs', 'root': '/', 'options': 'rw,relatime'})
        self.whole.extend([{'name': '/dev/vde', 'serial': 'tn-16e0a47bf6-data2', 'maj:min': '8:64', 'parent': None},
                           {'name': '/dev/nvme0n1', 'serial': 'tn-16e0a47bf6-cache', 'maj:min': '259:0', 'parent': None}])
        for disk in self.whole:
            disk.update(type='disk', size=audit.DISKS[disk['serial']], ro=False)
        self.partition = {'name': '/dev/vda1', 'maj:min': '8:1', 'type': 'part'}
        self.whole[0]['children'] = [self.partition]
        self.journal = {'schema': 1, 'spec': self.spec, 'stage': 'ready', 'pending': None,
                        'boot_id': harness.BOOT_ID, 'sync_completed_at': '2026-09-08T00:00:00Z',
                        'formatted': [{role: index} for role, index, _, _ in self.roles], 'detail': None}
        self.mounts.append({'target': str(harness.UNION), 'source': str(harness.DATA), 'major_minor': '0:71',
                            'filesystem': 'fuse.mergerfs', 'root': '/', 'options': 'rw'})
        self.options = {'branches': str(harness.DATA) + '=RW', 'category.create': 'mfs', 'cache.files': 'off',
                        'minfreespace': '21474836480', 'moveonenospc': 'mfs', 'func.getattr': 'newest'}
        self.directives = [f'parity {self.parity[0]}', f'2-parity {self.parity[1]}']
        self.directives += ['content ' + path for path in self.content] + [f'data d1 {harness.DATA}']
        self.directives += ['exclude ' + value for value in ['/lost+found/', '/tmp/', '*.unrecoverable', '.AppleDouble', '._AppleDouble', '.DS_Store']]
        self.directives += ['blocksize 256', 'autosave 500']
        self.texts = {'/sys/class/dmi/id/product_uuid': harness.VM_UUID,
                      '/proc/sys/kernel/random/boot_id': harness.BOOT_ID}
        self.hashes = {self.config: harness.CONFIG_SHA, '/usr/bin/snapraid': harness.BINARY_SHA}
        self.commands = []

    @property
    def raw(self):
        return json.dumps(self.journal).encode()

    def read_text(self, path):
        if str(path) == '/proc/self/mountinfo':
            return '\n'.join(f'{index + 1} 0 {row["major_minor"]} {row["root"]} {row["target"]} {row["options"]} - {row["filesystem"]} {row["source"]} rw'
                             for index, row in enumerate(self.mounts))
        return self.texts[str(path)]

    def command(self, args):
        self.commands.append(args)
        if args[0] == '/usr/bin/lsblk':
            output = json.dumps({'blockdevices': self.whole})
        elif args[:4] == ['/usr/sbin/blkid', '-p', '-o', 'export']:
            output = '\n'.join(key + '=' + value for key, value in self.probes[args[4]].items())
        elif args == ['/usr/bin/dpkg-query', '-W', '-f=${Package}=${Version}\n', 'snapraid', 'mergerfs']:
            output = 'snapraid=12.4-1\nmergerfs=2.40.2-5\n'
        else:
            raise AssertionError('Nieoczekiwany odczyt: ' + repr(args))
        return types.SimpleNamespace(returncode=0, stdout=output, stderr='')

    def small_file(self, path, *args):
        if str(path) == str(self.ROOT / f'{harness.ARRAY_ID}.json'):
            return self.raw
        if str(path) == self.config:
            return '\n'.join(self.directives).encode()
        raise AssertionError(path)

    @staticmethod
    def safe_parents(path):
        assert str(path).startswith('/mnt/tentanas-branches/')

    def file_metric(self, path, device):
        return {'present': True, 'sha256': self.hashes.get(str(path), 'metadata'), 'device': device}

    @contextlib.contextmanager
    def reads(self):
        with mock.patch.object(harness.os, 'geteuid', return_value=0), \
                mock.patch.object(Path, 'read_text', autospec=True, side_effect=self.read_text), \
                mock.patch.object(harness.os, 'stat', side_effect=lambda path, **kwargs: self.stats[str(path)]), \
                mock.patch.object(harness.os, 'statvfs', return_value=types.SimpleNamespace(f_bavail=2**20, f_frsize=4096)), \
                mock.patch.object(harness.os, 'getxattr', side_effect=lambda path, key: self.options[key.removeprefix('user.mergerfs.')].encode()), \
                mock.patch.object(harness, 'JOURNAL_SHA', hashlib.sha256(self.raw).hexdigest()):
            yield


class GuardTests(unittest.TestCase):
    def test_complete_inputs_reach_final_guard_with_three_distinct_devices(self):
        inputs = GuardInputs()
        with inputs.reads():
            result = harness.guard(inputs)
        self.assertEqual(len(inputs.whole), 6)
        self.assertEqual(len(set(result['devices'].values())), 3)
        self.assertEqual(result['binary_sha256'], harness.BINARY_SHA)
        self.assertEqual(result['union_device'], os.makedev(0, 71))
        self.assertEqual([args[4] for args in inputs.commands if args[0] == '/usr/sbin/blkid'], ['/dev/vdb', '/dev/vdc', '/dev/vdd'])

    def test_each_foreign_identity_or_measurement_is_refused(self):
        cases = [
            ('vm', lambda x: x.texts.update({'/sys/class/dmi/id/product_uuid': 'foreign'}), 'Obca VM'),
            ('boot', lambda x: x.texts.update({'/proc/sys/kernel/random/boot_id': 'foreign'}), 'nowego bootu'),
            ('array', lambda x: x.spec.update(array_id='00000000-0000-0000-0000-000000000001'), 'tożsamość'),
            ('operation', lambda x: x.spec.update(operation_id='00000000-0000-0000-0000-000000000001'), 'tożsamość'),
            ('pinned_uuid', lambda x: x.spec['data'][0].update(expected_uuid='00000000-0000-0000-0000-000000000001'), 'tożsamość'),
            ('probe_uuid', lambda x: x.probes['/dev/vdb'].update(UUID='foreign'), 'UUID/FS'),
            ('filesystem', lambda x: x.probes['/dev/vdb'].update(TYPE='ext4'), 'UUID/FS'),
            ('os_ancestor', lambda x: (x.whole[0].pop('children'), x.whole[1].update(children=[x.partition])), 'Obcy OS'),
            ('extra_disk', lambda x: x.whole.append(dict(x.whole[-1], name='/dev/foreign', serial='foreign')), 'sześciu'),
            ('disk_bytes', lambda x: x.whole[-1].update(size=123), 'seriale lub rozmiary'),
            ('mount_source', lambda x: x.mounts[0].update(source='/dev/vdz'), 'mount roli'),
            ('mount_readonly', lambda x: x.mounts[0].update(options='ro'), 'mount roli'),
            ('union_source', lambda x: x.mounts[-1].update(source='/foreign'), 'Obca unia'),
            ('union_duplicate', lambda x: x.mounts.append(copy.deepcopy(x.mounts[-1])), 'Obca unia'),
            ('nested_mount', lambda x: x.mounts.append(dict(x.mounts[-1], target=str(harness.UNION / 'foreign'))), 'Mount potomny'),
            ('options', lambda x: x.options.update(moveonenospc='false'), 'opcje unii'),
            ('config_paths', lambda x: x.directives.append('data foreign /tmp'), 'Inny config'),
            ('config_pin', lambda x: x.hashes.update({x.config: 'foreign'}), 'binarka/config'),
            ('binary_pin', lambda x: x.hashes.update({'/usr/bin/snapraid': 'foreign'}), 'binarka/config'),
        ]
        for name, change, error in cases:
            with self.subTest(name=name):
                inputs = GuardInputs()
                change(inputs)
                with inputs.reads(), self.assertRaisesRegex(ValueError, error):
                    harness.guard(inputs)

    def test_fixture_real_read_hash_and_execution_or_refusal_before_exec(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            fixture = base / 'audit.py'
            fixture.write_text('value = 73\n')
            fixture.chmod(0o600)
            real_fstat = os.fstat
            real_open = os.open

            def root_stat(fd):
                info = real_fstat(fd)
                return types.SimpleNamespace(st_mode=info.st_mode, st_uid=0, st_nlink=info.st_nlink)

            # Uprawnienia i odczyt pliku są rzeczywiste; własność roota i przodków to wejście pomiaru.
            with mock.patch.object(harness, 'FIXTURES', base), \
                    mock.patch.object(harness, 'PINS', {'audit.py': hashlib.sha256(fixture.read_bytes()).hexdigest()}), \
                    mock.patch.object(Path, 'lstat', return_value=types.SimpleNamespace(st_mode=stat.S_IFDIR | 0o700, st_uid=0)), \
                    mock.patch.object(harness.os, 'fstat', side_effect=root_stat), \
                    mock.patch.object(harness.os, 'open', wraps=real_open) as opened:
                self.assertEqual(harness.load_fixture('audit.py').value, 73)
                opened.assert_called_once_with(fixture, os.O_RDONLY | os.O_NOFOLLOW | os.O_NOATIME | os.O_CLOEXEC)
                fixture.write_text('raise AssertionError("Nie wolno wykonać obcego źródła")\n')
                with self.assertRaisesRegex(ValueError, 'SHA fixture'):
                    harness.load_fixture('audit.py')
                fixture.chmod(0o644)
                with self.assertRaisesRegex(ValueError, 'Obcy plik fixture'):
                    harness.load_fixture('audit.py')


if __name__ == '__main__':
    unittest.main()
