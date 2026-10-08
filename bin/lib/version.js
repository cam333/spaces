'use strict';

// Version order for the update check. Compared as strings, "0.10.0" sorts
// before "0.9.10" and "1.0.0-beta.2" after "1.0.0"; this compares the numbers.

function parts(v) {
  const [core, pre = ''] = String(v || '').replace(/^v/, '').split('-', 2);
  return { nums: core.split('.').map((n) => parseInt(n, 10) || 0), pre };
}

/** True when `candidate` is a later version than `current`. */
function isNewer(candidate, current) {
  if (!candidate || !current) return false;
  const a = parts(candidate);
  const b = parts(current);
  for (let i = 0; i < Math.max(a.nums.length, b.nums.length, 3); i++) {
    const d = (a.nums[i] || 0) - (b.nums[i] || 0);
    if (d) return d > 0;
  }
  // Same numbers: a release is newer than its pre-releases.
  if (a.pre === b.pre) return false;
  if (!a.pre) return true;
  if (!b.pre) return false;
  return a.pre.localeCompare(b.pre, undefined, { numeric: true }) > 0;
}

module.exports = { isNewer };
