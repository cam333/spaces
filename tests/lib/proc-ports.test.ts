import { describe, it, expect } from 'vitest';
import { listeningInodes, pidsListeningOn } from '../../bin/lib/proc-ports.js';

// Shape of /proc/net/tcp: local_address is hex IP:hex port, `st` 0A is LISTEN.
const TCP = `  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0F51 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 41234 1 0000000000000000 100 0 0 10 0
   1: 0100007F:0F51 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 41299 1 0000000000000000 20 4 30 10 -1
   2: 00000000:0D48 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 55555 1 0000000000000000 100 0 0 10 0
`;

describe('listeningInodes', () => {
  it('finds the inode of a socket listening on the port', () => {
    expect(listeningInodes(TCP, 3921)).toEqual(['41234']); // 0x0F51
  });

  it('ignores established connections on the same port', () => {
    expect(listeningInodes(TCP, 3921)).not.toContain('41299');
  });

  it('is empty for a port nobody listens on, and for garbage', () => {
    expect(listeningInodes(TCP, 80)).toEqual([]);
    expect(listeningInodes('not a proc file', 3921)).toEqual([]);
  });
});

describe('pidsListeningOn', () => {
  it('maps inodes to the processes holding them', () => {
    const fs = {
      readFileSync: (p: string) => {
        if (p === '/proc/net/tcp') return TCP;
        if (p === '/proc/net/tcp6') return '';
        throw new Error('ENOENT');
      },
      readdirSync: (p: string) => {
        if (p === '/proc') return ['1', '42', 'self', 'sys', '77'];
        if (p === '/proc/42/fd') return ['0', '1', '7'];
        if (p === '/proc/77/fd') return ['3'];
        throw new Error('EACCES');
      },
      readlinkSync: (p: string) => {
        if (p === '/proc/42/fd/7') return 'socket:[41234]';
        if (p === '/proc/77/fd/3') return 'socket:[55555]';
        return '/dev/null';
      },
    };
    expect(pidsListeningOn(3921, fs)).toEqual([42]);
  });

  it('returns nothing where /proc is unavailable', () => {
    const fs = {
      readFileSync: () => { throw new Error('ENOENT'); },
      readdirSync: () => { throw new Error('ENOENT'); },
      readlinkSync: () => { throw new Error('ENOENT'); },
    };
    expect(pidsListeningOn(3921, fs)).toEqual([]);
  });
});

describe('listenerUids', () => {
  it('names the account that owns each listener on the port', async () => {
    const { listenerUids } = await import('../../bin/lib/proc-ports.js');
    const fs = { readFileSync: (p: string) => (p === '/proc/net/tcp' ? TCP : '') };
    expect(listenerUids(3921, fs)).toEqual([1000]);
    expect(listenerUids(80, fs)).toEqual([]);
  });

  it('is empty where /proc is unavailable', async () => {
    const { listenerUids } = await import('../../bin/lib/proc-ports.js');
    expect(listenerUids(3921, { readFileSync: () => { throw new Error('ENOENT'); } })).toEqual([]);
  });
});
