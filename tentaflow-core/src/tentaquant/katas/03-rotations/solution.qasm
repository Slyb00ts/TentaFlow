OPENQASM 3.0;
include "stdgates.inc";

qubit[1] q;
bit[1] c;

ry(1.1592794807274085) q[0];

c = measure q;
