'use strict';

let _deps = null;

module.exports.init = function (deps) {
  _deps = deps;
};

module.exports.getDeps = function () {
  return _deps;
};
