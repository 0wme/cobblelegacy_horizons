#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
# was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Cobblemon 1.8.1's Pokémon posers written in Kotlin, as poser files in Cobblemon's own JSON format.

About a third of the species have no poser file: a Kotlin class builds their poses (`GengarModel`...), and
`VaryingModelRepository.registerInBuiltPosers` gives each class its poser name. This reads those classes and
writes, for each name, the file Cobblemon would have loaded for the same poser (`JsonPose`, `PoseAdapter`),
so that whatever reads the JSON posers reads these too.

    python tools/cobblelegacy/posers_from_kotlin.py <cobblemon source root> <output folder>

It writes `<poser>.json` for every name, `NOTICE.md` and `conversion-report.json` into the output folder
(and removes the poser files it no longer makes), and the Rust module that embeds them (`--module`, by
default `../../src/coded_posers.rs` from the output folder). A name Cobblemon also has a poser file for
gets no file: Cobblemon loads its file over the class, which is then never the poser.

Nothing is guessed. What the JSON format cannot say (an animation built from a lambda, a pose asked for by
something no key or MoLang query reads) is left out and listed in `conversion-report.json` with the Kotlin
it came from. The classes are read with a parser for the Kotlin they are written in, not run: a construct
this does not know stops that one animation, pose or class, and is reported the same way.

What a Kotlin poser does that its JSON does not say by default is written out:
- `transformToTicks` is 10 in `registerPose` and absent (the previous pose's `transformTicks`) in JSON;
- the animations built in code get every argument, from the Kotlin constructors' defaults (`sineFunction`
  is amplitude 1 and period 1 where `q.sine_wing_flap` is 0.9 and 0.9) and the parts the class binds;
- a quirk gets its seconds and its loops;
- a class's `cryAnimation` and `getFaintAnimation`, which are code Cobblemon asks after the named
  animations, become the named animations `cry` and `faint`, of the model or of the poses they ask about
  (`isPosedIn`), `q.in_battle` standing for `isBattling`.

Two things the two do not share:
- a Kotlin class registers its root part under the bone's own name, and Cobblemon's JSON loader registers
  it as `__root`. A transformed part that is the root is written with the bone's name, as the class has
  it and as the Bedrock animations of these species name it; `--json-root-name` writes `__root` instead,
  which is what Cobblemon itself would need to load the file;
- without an entity (a model drawn in a screen), `it.isInWater`, `it.isUnderWater`, `it.isInWaterOrRain`
  and `getEntity()?.isDusk() == true` fail in a class, and the keys they are written as (`isTouchingWater`
  and the like, true) hold in `PoseAdapter`. With an entity they are the same.
"""

import argparse
import bisect
from decimal import Decimal
import json
import os
from pathlib import Path
import re
import struct
import sys

MODELS = 'common/src/main/kotlin/com/cobblemon/mod/common/client/render/models/blockbench'
COMMON = 'common/src/main/kotlin/com/cobblemon/mod/common'
POSERS = 'common/src/main/resources/assets/cobblemon/bedrock/pokemon/posers'
ANIMATIONS = 'common/src/main/resources/assets/cobblemon/bedrock/pokemon/animations'
TOOL = 'tools/cobblelegacy/posers_from_kotlin.py'

# ---------------------------------------------------------------------------------------------------------
# Kotlin, as much of it as the model classes are written in.


class ParseError(Exception):
    def __init__(self, message, pos):
        super().__init__(message)
        self.pos = pos


class Tok:
    __slots__ = ('kind', 'text', 'pos', 'end', 'nl', 'value')

    def __init__(self, kind, text, pos, end, nl, value=None):
        self.kind, self.text, self.pos, self.end, self.nl, self.value = kind, text, pos, end, nl, value


PUNCT = [
    '===', '!==', '?.', '?:', '::', '->', '..', '&&', '||', '==', '!=', '<=', '>=', '+=', '-=', '*=', '/=', '!!',
    '(', ')', '{', '}', '[', ']', ',', '.', ':', ';', '=', '<', '>', '+', '-', '*', '/', '%', '!', '?', '@',
]
ID = re.compile(r'[A-Za-z_][A-Za-z0-9_]*')
NUMBER = re.compile(r'(?:(?:\d[\d_]*)?\.\d[\d_]*(?:[eE][+-]?\d+)?|\d[\d_]*(?:[eE][+-]?\d+)?)[fFL]?')
ESCAPES = {'n': '\n', 't': '\t', 'r': '\r', 'b': '\b', '"': '"', "'": "'", '\\': '\\', '$': '$'}


def read_string(src, i):
    """A string literal starting at `src[i]`: its parts, ('lit', text) or ('code', source), and where it ends."""
    n = len(src)
    if src.startswith('"""', i):
        end = src.find('"""', i + 3)
        if end < 0:
            raise ParseError('a raw string that does not end', i)
        return [('lit', src[i + 3:end])], end + 3
    parts, text, j = [], [], i + 1
    while True:
        if j >= n or src[j] == '\n':
            raise ParseError('a string that does not end', i)
        c = src[j]
        if c == '"':
            break
        if c == '\\':
            escaped = src[j + 1]
            if escaped == 'u':
                text.append(chr(int(src[j + 2:j + 6], 16)))
                j += 6
            else:
                text.append(ESCAPES[escaped])
                j += 2
        elif c == '$' and j + 1 < n and src[j + 1] == '{':
            depth, k = 1, j + 2
            while depth:
                if src[k] == '{':
                    depth += 1
                elif src[k] == '}':
                    depth -= 1
                k += 1
            if text:
                parts.append(('lit', ''.join(text)))
                text = []
            parts.append(('code', src[j + 2:k - 1]))
            j = k
        elif c == '$' and j + 1 < n and ID.match(src, j + 1):
            name = ID.match(src, j + 1)
            if text:
                parts.append(('lit', ''.join(text)))
                text = []
            parts.append(('code', name.group()))
            j = name.end()
        else:
            text.append(c)
            j += 1
    if text or not parts:
        parts.append(('lit', ''.join(text)))
    return parts, j + 1


def tokenize(src):
    toks, i, n, nl = [], 0, len(src), False
    while i < n:
        c = src[i]
        if c == '\n':
            nl = True
            i += 1
        elif c in ' \t\r':
            i += 1
        elif src.startswith('//', i):
            end = src.find('\n', i)
            i = n if end < 0 else end
        elif src.startswith('/*', i):
            # Kotlin's block comments nest.
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith('/*', i):
                    depth, i = depth + 1, i + 2
                elif src.startswith('*/', i):
                    depth, i = depth - 1, i + 2
                else:
                    nl = nl or src[i] == '\n'
                    i += 1
        elif c == '"':
            parts, end = read_string(src, i)
            toks.append(Tok('str', src[i:end], i, end, nl, parts))
            i, nl = end, False
        elif c == "'":
            end = i + 1
            while src[end] != "'":
                end += 2 if src[end] == '\\' else 1
            toks.append(Tok('char', src[i:end + 1], i, end + 1, nl))
            i, nl = end + 1, False
        elif c == '`':
            end = src.index('`', i + 1)
            toks.append(Tok('id', src[i + 1:end], i, end + 1, nl))
            i, nl = end + 1, False
        elif c.isdigit() or c == '.' and src[i + 1:i + 2].isdigit() and not (
                toks and (toks[-1].kind != 'op' or toks[-1].text in (')', ']', '}'))):
            # `.7F` is a number where nothing before it could have a member.
            m = NUMBER.match(src, i)
            toks.append(Tok('num', m.group(), i, m.end(), nl))
            i, nl = m.end(), False
        elif ID.match(src, i):
            m = ID.match(src, i)
            toks.append(Tok('id', m.group(), i, m.end(), nl))
            i, nl = m.end(), False
        else:
            for p in PUNCT:
                if src.startswith(p, i):
                    toks.append(Tok('op', p, i, i + len(p), nl))
                    i, nl = i + len(p), False
                    break
            else:
                raise ParseError(f'a character Kotlin has no use for here: {c!r}', i)
    toks.append(Tok('eof', '', n, n, True))
    return toks


class Node:
    """A piece of the syntax tree: its kind, where it is in the source, and what that kind has."""

    def __init__(self, k, s, e, **fields):
        self.k, self.s, self.e = k, s, e
        self.__dict__.update(fields)


MODIFIERS = {
    'override', 'private', 'protected', 'public', 'internal', 'open', 'lateinit', 'final', 'const', 'abstract',
    'inline', 'operator', 'infix',
}
INFIX = {'to', 'until', 'downTo', 'step', 'and', 'or', 'xor', 'shl', 'shr'}


class Parser:
    def __init__(self, src):
        self.src = src
        self.toks = tokenize(src)
        self.i = 0
        # Whether a line break ends a statement here: not between brackets.
        self.nls = [True]

    @property
    def tok(self):
        return self.toks[self.i]

    def peek(self, ahead=1):
        return self.toks[min(self.i + ahead, len(self.toks) - 1)]

    def at(self, text, kind='op'):
        return self.tok.kind == kind and self.tok.text == text

    def at_word(self, text):
        return self.at(text, 'id')

    def adv(self):
        tok = self.tok
        if tok.kind != 'eof':
            self.i += 1
        return tok

    def expect(self, text, kind='op'):
        if not self.at(text, kind):
            raise ParseError(f'`{text}` expected, `{self.tok.text}` found', self.tok.pos)
        return self.adv()

    def name(self):
        if self.tok.kind != 'id':
            raise ParseError(f'a name expected, `{self.tok.text}` found', self.tok.pos)
        return self.adv().text

    def nl(self):
        return self.nls[-1] and self.tok.nl

    def end(self):
        return self.toks[self.i - 1].end

    def skip(self, opening, closing):
        """Past a bracketed stretch, the brackets counted."""
        self.expect(opening)
        depth = 1
        while depth:
            tok = self.adv()
            if tok.kind == 'eof':
                raise ParseError(f'`{closing}` missing', tok.pos)
            if tok.kind == 'op' and tok.text == opening:
                depth += 1
            elif tok.kind == 'op' and tok.text == closing:
                depth -= 1

    # Types are only read past.
    def type_(self):
        start = self.tok.pos
        if self.at('('):
            self.skip('(', ')')
            if self.at('->'):
                self.adv()
                self.type_()
        else:
            self.name()
            while self.at('.') and self.peek().kind == 'id':
                self.adv()
                self.adv()
            if self.at('<'):
                self.skip('<', '>')
        while self.at('?'):
            self.adv()
        return self.src[start:self.end()]

    # Expressions, loosest binding first.
    def expr(self):
        return self.binary(self.conjunction, ('||',), True)

    def binary(self, operand, ops, after_newline=False):
        left = operand()
        while self.tok.kind == 'op' and self.tok.text in ops and (after_newline or not self.nl()):
            op = self.adv().text
            right = operand()
            left = Node('binary', left.s, right.e, op=op, l=left, r=right)
        return left

    def conjunction(self):
        return self.binary(self.equality, ('&&',), True)

    def equality(self):
        return self.binary(self.comparison, ('==', '!=', '===', '!=='))

    def comparison(self):
        return self.binary(self.named_check, ('<', '>', '<=', '>='))

    def named_check(self):
        left = self.elvis()
        while not self.nl():
            negated = self.at('!') and self.peek().kind == 'id' and self.peek().text in ('in', 'is')
            if not negated and not (self.tok.kind == 'id' and self.tok.text in ('in', 'is')):
                break
            if negated:
                self.adv()
            if self.adv().text == 'is':
                left = Node('is', left.s, self.end(), x=left, type=self.type_(), negated=negated)
            else:
                right = self.elvis()
                left = Node('binary', left.s, right.e, op='!in' if negated else 'in', l=left, r=right)
        return left

    def elvis(self):
        return self.binary(self.infix, ('?:',), True)

    def infix(self):
        left = self.range_()
        while self.tok.kind == 'id' and self.tok.text in INFIX and not self.nl():
            op = self.adv().text
            right = self.range_()
            left = Node('binary', left.s, right.e, op=op, l=left, r=right)
        return left

    def range_(self):
        return self.binary(self.additive, ('..',))

    def additive(self):
        return self.binary(self.multiplicative, ('+', '-'))

    def multiplicative(self):
        return self.binary(self.cast, ('*', '/', '%'))

    def cast(self):
        left = self.prefix()
        while self.at_word('as') and not self.nl():
            self.adv()
            safe = self.at('?')
            if safe:
                self.adv()
            left = Node('as', left.s, self.end(), x=left, type=self.type_(), safe=safe)
        return left

    def prefix(self):
        tok = self.tok
        if tok.kind == 'op' and tok.text in ('-', '+', '!'):
            self.adv()
            x = self.prefix()
            return Node('unary', tok.pos, x.e, op=tok.text, x=x)
        return self.postfix()

    def type_arguments_follow(self):
        """Whether the `<` here opens type arguments of a call: `arrayOf<PoseAnimation>(`."""
        depth, k = 0, 0
        while True:
            tok = self.peek(k)
            if tok.kind == 'op' and tok.text == '<':
                depth += 1
            elif tok.kind == 'op' and tok.text == '>':
                depth -= 1
                if not depth:
                    after = self.peek(k + 1)
                    return after.kind == 'op' and after.text == '('
            elif not (tok.kind == 'id' or tok.kind == 'op' and tok.text in (',', '.', '?', '*')):
                return False
            k += 1

    def postfix(self):
        node = self.primary()
        while True:
            tok = self.tok
            if tok.kind != 'op':
                break
            if tok.text in ('.', '?.'):
                self.adv()
                node = Node('member', node.s, self.tok.end, obj=node, name=self.name(), safe=tok.text == '?.')
            elif tok.text == '(' and not self.nl():
                args = self.args()
                lam = self.lambda_() if self.at('{') and not self.nl() else None
                node = Node('call', node.s, self.end(), fn=node, args=args, lam=lam)
            elif tok.text == '{' and not self.nl() and node.k in ('name', 'member'):
                lam = self.lambda_()
                node = Node('call', node.s, self.end(), fn=node, args=[], lam=lam)
            elif tok.text == '[' and not self.nl():
                self.adv()
                self.nls.append(False)
                args = [self.expr()]
                while self.at(','):
                    self.adv()
                    args.append(self.expr())
                self.nls.pop()
                self.expect(']')
                node = Node('index', node.s, self.end(), obj=node, args=args)
            elif tok.text == '!!':
                self.adv()
                node = Node('notnull', node.s, self.end(), x=node)
            elif tok.text == '<' and node.k in ('name', 'member') and self.type_arguments_follow():
                self.skip('<', '>')
            else:
                break
        return node

    def args(self):
        self.expect('(')
        self.nls.append(False)
        out = []
        while not self.at(')'):
            name = None
            if self.tok.kind == 'id' and self.peek().kind == 'op' and self.peek().text == '=':
                name = self.adv().text
                self.adv()
            if self.at('*'):
                self.adv()
            out.append((name, self.expr()))
            if self.at(','):
                self.adv()
            elif not self.at(')'):
                raise ParseError(f'`,` or `)` expected, `{self.tok.text}` found', self.tok.pos)
        self.nls.pop()
        self.expect(')')
        return out

    def primary(self):
        tok = self.tok
        if tok.kind == 'num':
            self.adv()
            text = tok.text.replace('_', '')
            if text[-1] in 'fF':
                return Node('num', tok.pos, tok.end, type='float', value=float(text[:-1]))
            if text[-1] == 'L':
                return Node('num', tok.pos, tok.end, type='int', value=int(text[:-1]))
            if '.' in text or 'e' in text or 'E' in text:
                return Node('num', tok.pos, tok.end, type='double', value=float(text))
            return Node('num', tok.pos, tok.end, type='int', value=int(text))
        if tok.kind == 'str':
            self.adv()
            parts = []
            for kind, text in tok.value:
                if kind == 'code':
                    inner = Parser(text)
                    code = inner.expr()
                    if inner.tok.kind != 'eof':
                        raise ParseError('a string template this cannot read', tok.pos)
                    # The node's place is the whole string's: it has no place of its own in the file.
                    parts.append(('code', code, text))
                else:
                    parts.append(('lit', text))
            return Node('str', tok.pos, tok.end, parts=parts)
        if tok.kind == 'op' and tok.text == '(':
            self.adv()
            self.nls.append(False)
            inner = self.expr()
            self.nls.pop()
            # The brackets are part of what the expression is written as.
            inner.s, inner.e = tok.pos, self.expect(')').end
            return inner
        if tok.kind == 'op' and tok.text == '{':
            return self.lambda_()
        if tok.kind == 'id':
            if tok.text in ('true', 'false'):
                self.adv()
                return Node('bool', tok.pos, tok.end, value=tok.text == 'true')
            if tok.text == 'null':
                self.adv()
                return Node('null', tok.pos, tok.end)
            if tok.text == 'this':
                self.adv()
                label = None
                if self.at('@') and self.tok.pos == tok.end:
                    self.adv()
                    label = self.name()
                return Node('this', tok.pos, self.end(), label=label)
            if tok.text == 'if':
                return self.if_()
            if tok.text == 'when':
                return self.when_()
            if tok.text == 'object':
                return self.object_()
            self.adv()
            return Node('name', tok.pos, tok.end, id=tok.text)
        raise ParseError(f'an expression expected, `{tok.text}` found', tok.pos)

    def lambda_(self):
        start = self.expect('{').pos
        self.nls.append(True)
        params = self.lambda_params()
        body = self.statements()
        self.nls.pop()
        return Node('lambda', start, self.expect('}').end, params=params, body=body)

    def lambda_params(self):
        """The names before a lambda's `->`, None for a lambda without them (its parameter is `it`)."""
        saved, names = self.i, []
        while self.tok.kind == 'id':
            names.append(self.adv().text)
            if self.at(':'):
                self.adv()
                try:
                    self.type_()
                except ParseError:
                    break
            if not self.at(','):
                break
            self.adv()
        if names and self.at('->'):
            self.adv()
            return names
        self.i = saved
        return None

    def statements(self):
        out = []
        while not self.at('}') and self.tok.kind != 'eof':
            if self.at(';'):
                self.adv()
            else:
                out.append(self.statement())
        return out

    def statement(self):
        tok = self.tok
        if tok.kind == 'id' and tok.text in ('val', 'var'):
            self.adv()
            name = self.name()
            if self.at(':'):
                self.adv()
                self.type_()
            init = None
            if self.at('='):
                self.adv()
                init = self.expr()
            return Node('val', tok.pos, self.end(), name=name, init=init, mods=[])
        if tok.kind == 'id' and tok.text == 'return':
            self.adv()
            value = None if self.nl() or self.at('}') else self.expr()
            return Node('return', tok.pos, self.end(), value=value)
        target = self.expr()
        if self.tok.kind == 'op' and self.tok.text in ('=', '+=', '-=', '*=', '/=') and not self.nl():
            op = self.adv().text
            value = self.expr()
            return Node('assign', target.s, value.e, target=target, op=op, value=value)
        return target

    def body(self):
        """What an `if` or a `when` branch runs: a block, or one expression."""
        if not self.at('{'):
            return self.statement()
        start = self.adv().pos
        self.nls.append(True)
        body = self.statements()
        self.nls.pop()
        return Node('block', start, self.expect('}').end, body=body)

    def if_(self):
        start = self.expect('if', 'id').pos
        self.expect('(')
        self.nls.append(False)
        cond = self.expr()
        self.nls.pop()
        self.expect(')')
        then = self.body()
        # The `else` may be on a later line.
        els = None
        if self.at_word('else'):
            self.adv()
            els = self.body()
        return Node('if', start, self.end(), cond=cond, then=then, els=els)

    def when_(self):
        start = self.expect('when', 'id').pos
        subject = None
        if self.at('('):
            self.adv()
            self.nls.append(False)
            subject = self.expr()
            self.nls.pop()
            self.expect(')')
        self.expect('{')
        self.nls.append(True)
        branches = []
        while not self.at('}'):
            if self.at(';'):
                self.adv()
                continue
            conds = None
            if self.at_word('else'):
                self.adv()
            else:
                conds = [self.expr()]
                while self.at(','):
                    self.adv()
                    conds.append(self.expr())
            self.expect('->')
            branches.append((conds, self.body()))
        self.nls.pop()
        return Node('when', start, self.expect('}').end, subject=subject, branches=branches)

    def object_(self):
        start = self.expect('object', 'id').pos
        supers = self.supertypes()
        return Node('object', start, self.end(), supers=supers, members=self.class_body())

    def supertypes(self):
        supers = []
        if self.at(':'):
            self.adv()
            while True:
                supers.append(self.type_())
                if self.at('('):
                    self.args()
                if not self.at(','):
                    break
                self.adv()
        return supers

    def params(self):
        """A function's or a constructor's parameter names."""
        self.expect('(')
        self.nls.append(False)
        names = []
        while not self.at(')'):
            while self.tok.kind == 'id' and (self.tok.text in MODIFIERS or self.tok.text in ('val', 'var', 'vararg')):
                self.adv()
            names.append(self.name())
            self.expect(':')
            self.type_()
            if self.at('='):
                self.adv()
                self.expr()
            if self.at(','):
                self.adv()
        self.nls.pop()
        self.expect(')')
        return names

    def class_body(self):
        self.expect('{')
        self.nls.append(True)
        members = []
        while not self.at('}'):
            if self.at(';'):
                self.adv()
            else:
                members.append(self.member())
        self.nls.pop()
        self.expect('}')
        return members

    def member(self):
        start = self.tok.pos
        mods = []
        while self.at('@'):
            self.adv()
            self.name()
            if self.at('('):
                self.skip('(', ')')
        while self.tok.kind == 'id' and self.tok.text in MODIFIERS:
            mods.append(self.adv().text)
        if self.at_word('val') or self.at_word('var'):
            self.adv()
            name = self.name()
            if self.at(':'):
                self.adv()
                self.type_()
            init = None
            if self.at('='):
                self.adv()
                init = self.expr()
            return Node('val', start, self.end(), name=name, init=init, mods=mods)
        if self.at_word('fun'):
            self.adv()
            name = self.name()
            params = self.params()
            if self.at(':'):
                self.adv()
                self.type_()
            if self.at('='):
                self.adv()
                body = self.expr()
            else:
                body = self.body()
            return Node('fun', start, self.end(), name=name, params=params, body=body, mods=mods)
        if self.at_word('init'):
            self.adv()
            return Node('init', start, self.end(), body=self.body())
        raise ParseError(f'a member of a class expected, `{self.tok.text}` found', self.tok.pos)

    def class_(self):
        start = self.expect('class', 'id').pos
        name = self.name()
        if self.at('<'):
            self.skip('<', '>')
        if self.at('('):
            self.params()
        supers = self.supertypes()
        members = self.class_body() if self.at('{') else []
        return Node('class', start, self.end(), name=name, supers=supers, members=members)


def parse_classes(src):
    """Every class of a Kotlin file."""
    parser = Parser(src)
    classes = []
    while parser.tok.kind != 'eof':
        after_a_dot = parser.i and parser.toks[parser.i - 1].text in ('::', '.')
        if parser.at_word('class') and not after_a_dot:
            classes.append(parser.class_())
        else:
            parser.adv()
    return classes


# ---------------------------------------------------------------------------------------------------------
# Numbers, as Kotlin computes them and as JSON and MoLang write them.

# `RADIAN_IN_DEGREES` of `AngleExtensions.kt`, which is how `toRadians` converts.
RADIAN_IN_DEGREES = 57.2958


def f32(value):
    """The `Float` nearest to a number."""
    return struct.unpack('<f', struct.pack('<f', value))[0]


def f32_neighbour(value, steps):
    """The `Float` `steps` floats above (or below) one."""
    bits = struct.unpack('<i', struct.pack('<f', value))[0]
    bits += steps if bits >= 0 else -steps
    return struct.unpack('<f', struct.pack('<i', bits))[0]


class Num:
    """A Kotlin number: `type` is int, float or double, and a float's value is kept as that `Float`."""

    __slots__ = ('type', 'value')

    def __init__(self, type_, value):
        self.type = type_
        self.value = f32(value) if type_ == 'float' else value

    def to_float(self):
        return Num('float', self.value)

    def written(self):
        """The shortest decimal that reads back as this number, to the `Float` it is when it is one."""
        if self.type == 'int':
            return self.value
        if self.type == 'float':
            for digits in range(1, 10):
                short = float('%.*g' % (digits, self.value))
                if f32(short) == self.value:
                    return short
        return self.value


def decimal(value):
    """A number as JSON and MoLang both read it: digits, at most one dot, no exponent."""
    if isinstance(value, Num):
        value = value.written()
    if isinstance(value, bool):
        raise TypeError('a boolean is no number')
    if isinstance(value, int):
        return str(value)
    if value == int(value) and abs(value) < 1e15:
        return str(int(value))
    text = repr(value)
    if 'e' in text or 'E' in text:
        text = format(Decimal(text), 'f')
    return text


def arithmetic(op, left, right):
    kinds = (left.type, right.type)
    type_ = 'double' if 'double' in kinds else 'float' if 'float' in kinds else 'int'
    a, b = left.value, right.value
    if op == '+':
        value = a + b
    elif op == '-':
        value = a - b
    elif op == '*':
        value = a * b
    elif op == '/':
        if type_ == 'int':
            value = abs(a) // abs(b) * (1 if (a < 0) == (b < 0) else -1)
        else:
            value = a / b
    else:
        raise ValueError(op)
    return Num(type_, value)


# ---------------------------------------------------------------------------------------------------------
# What the classes' expressions are worth.


class Cannot(Exception):
    """An expression this does not know the worth of: why, and the syntax it stopped at."""

    def __init__(self, reason, node=None, kind='not_understood'):
        super().__init__(reason)
        self.reason, self.node, self.kind = reason, node, kind


class Part:
    """A part of the model (`getPart`), or its root part."""

    def __init__(self, name, root=False):
        self.name, self.root = name, root


class Frame:
    """The model itself, or an `object : HeadedFrame { ... }`: the parts it gives names to."""

    def __init__(self):
        self.fields = {}


class Root:
    """The constructor's `root`."""


class PoseType:
    def __init__(self, name):
        self.name = name


class PoseSet(list):
    """A set of pose types, in the order Kotlin's set iterates."""


class Pair:
    def __init__(self, a, b):
        self.a, self.b = a, b


class Range:
    def __init__(self, a, b):
        self.a, self.b = a, b


class ExpressionLike:
    """`"...".asExpressionLike()`: MoLang."""

    def __init__(self, source):
        self.source = source


class Lambda:
    def __init__(self, node, env):
        self.node, self.env = node, env


class PoseRef:
    """What `registerPose` returns."""

    def __init__(self, name):
        self.name = name


class Unassigned:
    """A `lateinit var` nothing was put in."""

    def __init__(self, name):
        self.name = name


class Stateful:
    """`bedrockStateful(group, animation)`."""

    def __init__(self, group, name):
        self.group, self.name = group, name


class Primary:
    """`PrimaryAnimation(bedrockStateful(group, animation))`, with its default curve and no label."""

    def __init__(self, animation):
        self.animation = animation


class Animation:
    """A pose animation the JSON format has a call for."""

    def __init__(self, kind, molang, parts=()):
        self.kind, self.molang, self.parts, self.condition = kind, molang, list(parts), None


class Wave:
    """A wave function (`sineFunction`...)."""

    def __init__(self, function, params):
        self.function, self.params = function, params


class Vec3:
    def __init__(self, x, y, z):
        self.xyz = [x, y, z]


class Opaque:
    """Something only an animation the JSON format lacks takes (`WaveSegment`)."""

    def __init__(self, what):
        self.what = what


class Namespace:
    """An object whose members are constants (`DataKeys`, `PoseType`)."""

    def __init__(self, name, members):
        self.name, self.members = name, members


class Poisoned:
    """A `val` whose value this could not work out: using it fails the same way."""

    def __init__(self, cannot):
        self.cannot = cannot


class Transformation:
    """`ModelPartTransformation`, with its arithmetic in `Float` as the class does it."""

    def __init__(self, part):
        self.part = part
        self.position = [0.0, 0.0, 0.0]
        self.rotation = [0.0, 0.0, 0.0]
        # For each axis, the degrees its rotation was given as, while that is all it is made of.
        self.degrees = [0.0, 0.0, 0.0]
        self.scale = [1.0, 1.0, 1.0]
        self.visibility = None

    def set_rotation(self, axis, radians, degrees=None):
        self.rotation[axis] = f32(radians)
        self.degrees[axis] = degrees


class Quirk:
    def __init__(self, molang):
        self.molang = molang


AXES = 'xyz'
QUOTE = "'"


def quoted(text, node=None):
    """A MoLang string. Its tokenizer has no escape that takes a quote out of one."""
    if QUOTE in text or '\\' in text:
        raise Cannot(f'a name with a quote or a backslash in it: {text}', node)
    return QUOTE + text + QUOTE


def is_name(node, name):
    return node.k == 'name' and node.id == name


class Model:
    """One Kotlin model class, read into the poser it builds."""

    def __init__(self, source, cls, context):
        self.src, self.cls, self.context = source, cls, context
        self.line_starts = [0] + [m.end() for m in re.finditer('\n', source)]
        self.frame = Frame()
        self.env = {'root': Root()}
        self.root_bone = None
        self.portrait = {}
        self.named = {}
        self.properties = {}
        self.poses = {}
        self.cry = None
        self.faint = None
        self.dropped = []
        self.notes = []
        # The pose a problem is in, for the report.
        self.where = 'model'

    # Reporting.
    def snippet(self, node):
        return self.src[node.s:node.e].strip() if node is not None else ''

    def line(self, node):
        if node is None:
            return None
        return bisect.bisect_right(self.line_starts, node.s)

    def drop(self, what, kind, reason, node, choice=None, where=None):
        entry = {
            'where': where or self.where,
            'what': what,
            'kind': kind,
            'reason': reason,
            'kotlin': self.snippet(node),
            'line': self.line(node),
        }
        if choice:
            entry['choice'] = choice
        self.dropped.append(entry)

    def note(self, kind, text, node=None, where=None):
        entry = {'where': where or self.where, 'kind': kind, 'note': text}
        if node is not None:
            entry['kotlin'] = self.snippet(node)
            entry['line'] = self.line(node)
        self.notes.append(entry)

    # Evaluation.
    def lookup(self, name, env, node):
        scope = env
        while scope is not None:
            if name in scope:
                value = scope[name]
                if isinstance(value, Poisoned):
                    raise Cannot(value.cannot.reason, value.cannot.node or node, value.cannot.kind)
                return value
            scope = scope.get('#outer')
        if name in self.context.constants:
            return self.context.constants[name]
        raise Cannot(f'`{name}` is nothing this knows', node)

    def ev(self, node, env):
        k = node.k
        if k == 'num':
            return Num(node.type, node.value)
        if k == 'str':
            text = []
            for part in node.parts:
                if part[0] == 'lit':
                    text.append(part[1])
                else:
                    try:
                        value = self.ev(part[1], env)
                    except Cannot as cannot:
                        raise Cannot(cannot.reason, node, cannot.kind) from None
                    if not isinstance(value, str):
                        raise Cannot(f'`{part[2]}` in a string is not a string', node)
                    text.append(value)
            return ''.join(text)
        if k == 'bool':
            return node.value
        if k == 'null':
            return None
        if k == 'name':
            return self.lookup(node.id, env, node)
        if k == 'this':
            return self.frame
        if k == 'lambda':
            return Lambda(node, env)
        if k == 'as':
            return self.ev(node.x, env)
        if k == 'notnull':
            return self.ev(node.x, env)
        if k == 'unary':
            value = self.ev(node.x, env)
            if node.op == '-' and isinstance(value, Num):
                return Num(value.type, -value.value)
            if node.op == '+' and isinstance(value, Num):
                return value
            if node.op == '!' and isinstance(value, bool):
                return not value
            raise Cannot(f'`{node.op}` of this', node)
        if k == 'binary':
            return self.binary(node, env)
        if k == 'member':
            return self.member(node, env)
        if k == 'call':
            return self.call(node, env)
        if k == 'object':
            return self.object_(node, env)
        raise Cannot(f'a `{k}` expression', node)

    def binary(self, node, env):
        left, right, op = self.ev(node.l, env), self.ev(node.r, env), node.op
        if isinstance(left, Num) and isinstance(right, Num) and op in '+-*/':
            return arithmetic(op, left, right)
        if op == 'to':
            return Pair(left, right)
        if op == '..':
            return Range(left, right)
        # `Set.plus` and `Set.minus` make a `LinkedHashSet`: the left set's order, then what is new.
        if isinstance(left, PoseSet) and op in '+-':
            others = [t.name for t in right] if isinstance(right, PoseSet) else None
            if isinstance(right, PoseType):
                others = [right.name]
            if others is not None:
                names = [t.name for t in left]
                if op == '+':
                    names += [name for name in others if name not in names]
                else:
                    names = [name for name in names if name not in others]
                return PoseSet(PoseType(name) for name in names)
        raise Cannot(f'`{op}` between these', node)

    def member(self, node, env):
        obj = self.ev(node.obj, env)
        name = node.name
        if isinstance(obj, Namespace):
            if name == 'Companion':
                return obj
            if name in obj.members:
                return obj.members[name]
            raise Cannot(f'`{obj.name}.{name}` is nothing this knows', node)
        if isinstance(obj, Frame):
            if name in obj.fields:
                return obj.fields[name]
            return self.lookup(name, self.env, node) if obj is self.frame else self.fail(node)
        if isinstance(obj, PoseRef) and name == 'poseName':
            return obj.name
        if isinstance(obj, Unassigned):
            raise Cannot(f'`{obj.name}` is read before any pose is put in it', node, 'unassigned_pose')
        return self.fail(node)

    def fail(self, node):
        raise Cannot(f'`{self.snippet(node)}` is nothing this knows', node)

    def object_(self, node, env):
        frame = Frame()
        for member in node.members:
            if member.k != 'val' or member.init is None:
                raise Cannot('an object with more than values in it', member)
            frame.fields[member.name] = self.ev(member.init, env)
        return frame

    def bind(self, node, names):
        """A call's arguments by parameter name, still unevaluated; a trailing lambda is the last one."""
        bound, position = {}, 0
        for name, value in node.args:
            if name is None:
                if position >= len(names):
                    raise Cannot('more arguments than the function takes', node)
                name = names[position]
                position += 1
            elif name not in names:
                raise Cannot(f'no parameter is called `{name}`', node)
            bound[name] = value
        if node.lam is not None:
            bound[names[-1]] = node.lam
        return bound

    def number(self, bound, name, env, default):
        if name not in bound:
            return default
        value = self.ev(bound[name], env)
        if value is None:
            return default
        if not isinstance(value, Num):
            raise Cannot(f'`{name}` is not a number', bound[name])
        return value

    def string(self, node, env):
        value = self.ev(node, env)
        if not isinstance(value, str):
            raise Cannot('not a string', node)
        return value

    def call(self, node, env):
        fn = node.fn
        if fn.k == 'name':
            handler = getattr(self, 'fn_' + fn.id, None)
            if handler is None:
                raise Cannot(f'the function `{fn.id}` is nothing this knows', node)
            return handler(node, env)
        if fn.k != 'member':
            raise Cannot('a call of something that is not a name', node)
        receiver = self.ev(fn.obj, env)
        if receiver is None and fn.safe:
            return None
        return self.method(receiver, fn.name, node, env)

    def method(self, receiver, name, node, env):
        if isinstance(receiver, Root) and name == 'registerChildWithAllChildren':
            return Part(self.string(node.args[0][1], env), root=True)
        if isinstance(receiver, str) and name == 'asExpressionLike':
            return ExpressionLike(receiver)
        if isinstance(receiver, Num):
            if name == 'toRadians':
                return Num('float', f32(receiver.value) / f32(RADIAN_IN_DEGREES))
            if name == 'toDegrees':
                return Num('float', f32(receiver.value) * f32(RADIAN_IN_DEGREES))
            if name == 'toFloat':
                return receiver.to_float()
            if name == 'toDouble':
                return Num('double', float(receiver.value))
            if name == 'toInt':
                return Num('int', int(receiver.value))
        if isinstance(receiver, Part):
            if name == 'createTransformation':
                return Transformation(receiver)
            if name in ('translation', 'rotation'):
                made = 'TranslationFunctionPoseAnimation' if name == 'translation' else 'RotationFunctionPoseAnimation'
                raise Cannot(
                    f'`{made}` (a part moved along a wave function by a time lambda): no MoLang function makes one',
                    node, 'function_animation')
        if isinstance(receiver, Transformation):
            return self.transform(receiver, name, node, env)
        if isinstance(receiver, Frame):
            if name == 'singleBoneLook':
                return self.look_of_frame(receiver, self.bind(node, LOOK_OF_FRAME), env, node)
            if name == 'wingFlap':
                bound = self.bind(node, ['flapFunction', 'timeVariable', 'axis'])
                return self.wing_flap(receiver, bound, 'flapFunction', env, node)
        if isinstance(receiver, Animation) and name == 'withCondition':
            condition = self.ev(node.args[0][1], env) if len(node.args) == 1 else None
            if not isinstance(condition, ExpressionLike):
                raise Cannot('`withCondition` with a lambda: an animation of a JSON pose takes MoLang', node,
                             'animation_condition')
            receiver.condition = condition.source
            return receiver
        raise Cannot(f'`.{name}` of this is nothing this knows', node)

    # The model's own functions.
    def fn_getPart(self, node, env):
        return Part(self.string(node.args[0][1], env))

    def bedrock_names(self, node, env):
        bound = self.bind(node, ['animationGroup', 'animation', 'animationPrefix'])
        group, name = self.string(bound['animationGroup'], env), self.string(bound['animation'], env)
        if 'animationPrefix' in bound and self.string(bound['animationPrefix'], env) != f'animation.{group}':
            raise Cannot('an animation prefix of its own: `q.bedrock` takes none', node, 'animation_prefix')
        return group, name

    def fn_bedrock(self, node, env):
        group, name = self.bedrock_names(node, env)
        return Animation('bedrock', f'q.bedrock({quoted(group, node)}, {quoted(name, node)})')

    def fn_bedrockStateful(self, node, env):
        return Stateful(*self.bedrock_names(node, env))

    def fn_PrimaryAnimation(self, node, env):
        bound = self.bind(node, ['animation', 'curve', 'excludedLabels', 'isTransition'])
        animation = self.ev(bound['animation'], env)
        if len(bound) != 1 or not isinstance(animation, Stateful):
            raise Cannot('a `PrimaryAnimation` of more than a `bedrockStateful`', node)
        return Primary(animation)

    def fn_CryProvider(self, node, env):
        return Lambda(node.lam, env)

    def fn_Vec3(self, node, env):
        values = [self.ev(value, env) for _, value in node.args]
        if len(values) != 3 or not all(isinstance(v, Num) for v in values):
            raise Cannot('a `Vec3` of something else than three numbers', node)
        return Vec3(*values)

    def fn_arrayOf(self, node, env):
        return [self.ev(value, env) for _, value in node.args]

    fn_listOf = fn_mutableListOf = fn_arrayOf

    def fn_emptyArray(self, node, env):
        return []

    def fn_setOf(self, node, env):
        values = [self.ev(value, env) for _, value in node.args]
        if values and all(isinstance(v, PoseType) for v in values):
            names = []
            for value in values:
                if value.name not in names:
                    names.append(value.name)
            return PoseSet(PoseType(name) for name in names)
        return values

    def fn_mutableMapOf(self, node, env):
        out = {}
        for _, value in node.args:
            pair = self.ev(value, env)
            if not isinstance(pair, Pair) or not isinstance(pair.a, str):
                raise Cannot('a map of something else than `"name" to value`', node)
            out[pair.a] = pair.b
        return out

    fn_mapOf = fn_mutableMapOf

    def fn_WaveSegment(self, node, env):
        return Opaque('WaveSegment')

    def fn_WaveAnimation(self, node, env):
        raise Cannot('`WaveAnimation` (a chain of parts bent along a wave): no MoLang function makes one', node,
                     'wave_animation')

    def wave(self, function, node, env):
        bound = self.bind(node, WAVES[function])
        return Wave(function, {name: self.ev(value, env) for name, value in bound.items()})

    def fn_sineFunction(self, node, env):
        return self.wave('sineFunction', node, env)

    def fn_cosineFunction(self, node, env):
        return self.wave('cosineFunction', node, env)

    def fn_triangleFunction(self, node, env):
        return self.wave('triangleFunction', node, env)

    def fn_parabolaFunction(self, node, env):
        return Wave('parabolaFunction', {})

    def fn_linearFunction(self, node, env):
        return Wave('linearFunction', {})

    # The animations Cobblemon computes.
    def frame_or(self, bound, first, env):
        """The frame a constructor is given as its first argument, None when it is given parts."""
        value = self.ev(bound[first], env) if first in bound else None
        return value if isinstance(value, Frame) else None

    def part_of(self, frame, field, node):
        part = frame.fields.get(field) if isinstance(frame, Frame) else None
        if not isinstance(part, Part):
            raise Cannot(f'the model binds no `{field}`', node)
        return part.name

    def part_arg(self, bound, name, env, node):
        if name not in bound:
            raise Cannot(f'no `{name}`', node)
        part = self.ev(bound[name], env)
        if not isinstance(part, Part):
            raise Cannot(f'`{name}` is not a part of the model', bound[name])
        return part.name

    def fn_singleBoneLook(self, node, env):
        return self.look_of_frame(self.frame, self.bind(node, LOOK_OF_FRAME), env, node)

    def look_of_frame(self, frame, bound, env, node):
        """`SingleBoneLookAnimation(frame, invertX, invertY, disableX, disableY, ...)`."""
        def flag(name):
            value = self.ev(bound[name], env) if name in bound else False
            if not isinstance(value, bool):
                raise Cannot(f'`{name}` is not true or false', node)
            return value

        pitch = Num('float', 0.0 if flag('disableX') else -1.0 if flag('invertX') else 1.0)
        yaw = Num('float', 0.0 if flag('disableY') else -1.0 if flag('invertY') else 1.0)
        return self.look(self.part_of(frame, 'head', node), [
            self.number(bound, 'pitchMultiplier', env, pitch),
            self.number(bound, 'yawMultiplier', env, yaw),
            self.number(bound, 'maxPitch', env, Num('float', 70.0)),
            self.number(bound, 'minPitch', env, Num('float', -45.0)),
            self.number(bound, 'maxYaw', env, Num('float', 45.0)),
            self.number(bound, 'minYaw', env, Num('float', -45.0)),
        ], node)

    def look(self, bone, numbers, node):
        written = ', '.join(decimal(n.to_float()) for n in numbers)
        return Animation('look', f'q.look({quoted(bone, node)}, {written})', [bone])

    def fn_SingleBoneLookAnimation(self, node, env):
        if node.args and self.frame_or({'frame': node.args[0][1]}, 'frame', env) is not None:
            bound = self.bind(node, ['frame'] + LOOK_OF_FRAME)
            return self.look_of_frame(self.ev(bound['frame'], env), bound, env, node)
        bound = self.bind(
            node, ['bone', 'pitchMultiplier', 'yawMultiplier', 'maxPitch', 'minPitch', 'maxYaw', 'minYaw'])
        return self.look(self.part_arg(bound, 'bone', env, node), [
            self.number(bound, 'pitchMultiplier', env, Num('float', 1.0)),
            self.number(bound, 'yawMultiplier', env, Num('float', 1.0)),
            self.number(bound, 'maxPitch', env, Num('float', 70.0)),
            self.number(bound, 'minPitch', env, Num('float', -45.0)),
            self.number(bound, 'maxYaw', env, Num('float', 45.0)),
            self.number(bound, 'minYaw', env, Num('float', -45.0)),
        ], node)

    def walk(self, node, env, function, period_name, amplitude, fields, by_frame, by_part):
        """An animation whose class has a constructor that takes a frame and one that takes the parts."""
        first = node.args[0] if node.args else None
        takes_frame = first is not None and (first[0] == 'frame' or first[0] is None and isinstance(
            self.ev(first[1], env), Frame))
        bound = self.bind(node, by_frame if takes_frame else by_part)
        if takes_frame:
            frame = self.ev(bound['frame'], env)
            parts = [self.part_of(frame, field, node) for field, _ in fields]
        else:
            parts = [self.part_arg(bound, parameter, env, node) for _, parameter in fields]
        numbers = [
            self.number(bound, period_name, env, Num('float', 0.6662)),
            self.number(bound, 'amplitudeMultiplier', env, Num('float', amplitude)),
        ]
        written = [decimal(n.to_float()) for n in numbers] + [quoted(part, node) for part in parts]
        return Animation(function, f'q.{function}({", ".join(written)})', parts)

    def fn_BipedWalkAnimation(self, node, env):
        return self.walk(
            node, env, 'biped_walk', 'periodMultiplier', 1.4, [('leftLeg', 'leftLeg'), ('rightLeg', 'rightLeg')],
            ['frame', 'periodMultiplier', 'amplitudeMultiplier'],
            ['periodMultiplier', 'amplitudeMultiplier', 'leftLeg', 'rightLeg'])

    def fn_QuadrupedWalkAnimation(self, node, env):
        return self.walk(
            node, env, 'quadruped_walk', 'periodMultiplier', 1.4,
            [('foreLeftLeg', 'legFrontLeft'), ('foreRightLeg', 'legFrontRight'), ('hindLeftLeg', 'legBackLeft'),
             ('hindRightLeg', 'legBackRight')],
            ['frame', 'periodMultiplier', 'amplitudeMultiplier'],
            ['legFrontLeft', 'legFrontRight', 'legBackLeft', 'legBackRight', 'periodMultiplier',
             'amplitudeMultiplier'])

    def fn_BimanualSwingAnimation(self, node, env):
        return self.walk(
            node, env, 'bimanual_swing', 'swingPeriodMultiplier', 1.0,
            [('leftArm', 'leftArm'), ('rightArm', 'rightArm')],
            ['frame', 'swingPeriodMultiplier', 'amplitudeMultiplier'],
            ['swingPeriodMultiplier', 'amplitudeMultiplier', 'leftArm', 'rightArm'])

    def fn_wingFlap(self, node, env):
        bound = self.bind(node, ['flapFunction', 'timeVariable', 'axis'])
        return self.wing_flap(self.frame, bound, 'flapFunction', env, node)

    def fn_WingFlapIdleAnimation(self, node, env):
        first = node.args[0] if node.args else None
        if first is not None and (
                first[0] == 'frame' or first[0] is None and isinstance(self.ev(first[1], env), Frame)):
            bound = self.bind(node, ['frame', 'flapFunction', 'timeVariable', 'axis'])
            return self.wing_flap(self.ev(bound['frame'], env), bound, 'flapFunction', env, node)
        bound = self.bind(node, ['leftWing', 'rightWing', 'rotation', 'timeVariable', 'axis'])
        wings = [self.part_arg(bound, 'leftWing', env, node), self.part_arg(bound, 'rightWing', env, node)]
        return self.wing_flap(wings, bound, 'rotation', env, node)

    def wing_flap(self, wings, bound, wave_name, env, node):
        """`WingFlapIdleAnimation`, which `q.sine_wing_flap` makes over a sine of the state's seconds only."""
        if isinstance(wings, Frame):
            wings = [self.part_of(wings, 'leftWing', node), self.part_of(wings, 'rightWing', node)]
        wave = self.ev(bound[wave_name], env)
        if not isinstance(wave, Wave):
            raise Cannot('a wing flap over something that is no wave function', node)
        if wave.function != 'sineFunction':
            raise Cannot(
                f'a wing flap over `{wave.function}`: `q.sine_wing_flap` only flaps along a sine', node,
                'wing_flap_not_a_sine')
        for name, value in wave.params.items():
            if not isinstance(value, Num):
                raise Cannot(f'a sine whose `{name}` is no number', node)
        phase = wave.params.get('phaseShift')
        if phase is not None and phase.value != 0:
            raise Cannot('a wing flap over a sine with a phase shift: `q.sine_wing_flap` has none', node,
                         'wing_flap_phase_shift')
        if 'timeVariable' in bound and not self.is_animation_seconds(bound['timeVariable']):
            raise Cannot(
                'a wing flap timed by something else than `state.animationSeconds`, which is all '
                '`q.sine_wing_flap` goes by', node, 'wing_flap_time')
        axis = self.ev(bound['axis'], env) if 'axis' in bound else None
        if not isinstance(axis, Num) or axis.value not in (0, 1, 2):
            raise Cannot('a wing flap around no axis this knows', node)
        # `sineFunction(amplitude = 1F, period = 1F, phaseShift = 0F, verticalShift = 0F)`.
        one, zero = Num('float', 1.0), Num('float', 0.0)
        written = [
            decimal(wave.params.get('amplitude', one).to_float()),
            decimal(wave.params.get('period', one).to_float()),
            decimal(wave.params.get('verticalShift', zero).to_float()),
            quoted(AXES[axis.value]),
        ] + [quoted(wing, node) for wing in wings]
        return Animation('sine_wing_flap', f'q.sine_wing_flap({", ".join(written)})', wings)

    @staticmethod
    def is_animation_seconds(node):
        """`{ state, _, _ -> state.animationSeconds }`, or that with `?:` a zero behind it."""
        if node.k != 'lambda' or not node.params or len(node.params) != 3 or len(node.body) != 1:
            return False
        body = node.body[0]
        if body.k == 'binary' and body.op == '?:' and body.r.k == 'num' and body.r.value == 0:
            body = body.l
        return body.k == 'member' and body.name == 'animationSeconds' and is_name(body.obj, node.params[0])

    # Quirks.
    def fn_quirk(self, node, env):
        bound = self.bind(node, ['secondsBetweenOccurrences', 'loopTimes', 'condition', 'animation'])
        seconds = self.ev(bound['secondsBetweenOccurrences'], env) if 'secondsBetweenOccurrences' in bound else Pair(
            Num('float', 8.0), Num('float', 30.0))
        if not isinstance(seconds, Pair) or not isinstance(seconds.a, Num) or not isinstance(seconds.b, Num):
            raise Cannot('a quirk whose seconds are not `a to b`', node)
        loops = self.ev(bound['loopTimes'], env) if 'loopTimes' in bound else Range(Num('int', 1), Num('int', 1))
        if not isinstance(loops, Range) or not isinstance(loops.a, Num) or not isinstance(loops.b, Num):
            raise Cannot('a quirk whose loops are not a range', node)
        if loops.a.value != 1:
            raise Cannot(
                f'a quirk that loops {loops.a.value} to {loops.b.value} times: `q.bedrock_quirk` loops from 1 '
                'to a number of times', node, 'quirk_loops')
        if 'condition' in bound:
            condition = bound['condition']
            always = condition.k == 'lambda' and len(condition.body) == 1 and condition.body[0].k == 'bool' and \
                condition.body[0].value
            if not always:
                raise Cannot('a quirk with a condition: `q.bedrock_quirk` makes one that always may run', node,
                             'quirk_condition')
        animation = bound.get('animation')
        if animation is None or animation.k != 'lambda' or len(animation.body) != 1:
            raise Cannot('a quirk whose animation is not one expression', node)
        played = self.ev(animation.body[0], {'#outer': env, (animation.params or ['it'])[0]: Opaque('state')})
        function = 'bedrock_quirk'
        if isinstance(played, Primary):
            function, played = 'bedrock_primary_quirk', played.animation
        if not isinstance(played, Stateful):
            raise Cannot('a quirk that plays something else than a `bedrockStateful`', node)
        written = [
            quoted(played.group, node), quoted(played.name, node), decimal(seconds.a.to_float()),
            decimal(seconds.b.to_float()), decimal(loops.b),
        ]
        return Quirk(f'q.{function}({", ".join(written)})')

    def fn_quirkMultiple(self, node, env):
        raise Cannot('`quirkMultiple`: several animations at once, where `q.bedrock_quirk` picks one', node,
                     'quirk_multiple')

    # Transformed parts.
    def transform(self, t, name, node, env):
        values = [self.ev(value, env) for _, value in node.args]
        numbers = all(isinstance(v, Num) for v in values)

        def radians(degrees):
            return f32(f32(degrees.value) / f32(RADIAN_IN_DEGREES))

        if name == 'withVisibility' and len(values) == 1:
            if isinstance(values[0], bool):
                t.visibility = values[0]
            elif isinstance(values[0], ExpressionLike):
                t.visibility = values[0].source
            else:
                raise Cannot('a visibility that is neither a boolean nor MoLang', node)
        elif name in ('withPosition', 'addPosition') and numbers and len(values) in (2, 3):
            base = [0.0] * 3 if name == 'withPosition' else t.position
            for axis, value in self.by_axis(values, node):
                t.position[axis] = f32(base[axis] + f32(value.value))
        elif name in ('withRotation', 'addRotation') and numbers and len(values) in (2, 3):
            for axis, value in self.by_axis(values, node):
                base = 0.0 if name == 'withRotation' else t.rotation[axis]
                t.set_rotation(axis, base + f32(value.value))
        elif name == 'withRotationDegrees' and numbers and len(values) == 3:
            for axis, value in enumerate(values):
                t.set_rotation(axis, radians(value), f32(value.value))
        elif name == 'addRotationDegrees' and numbers and len(values) == 3:
            for axis, value in enumerate(values):
                was = t.rotation[axis]
                t.set_rotation(axis, was + radians(value), f32(value.value) if was == 0.0 else None)
        elif name == 'addRotationDegrees' and numbers and len(values) == 2:
            # As written in `ModelPartTransformation`: `addRotation(axis, rotation[axis] + angle.toRadians())`,
            # so what the part was already turned by counts twice.
            (axis, value), = self.by_axis(values, node)
            was = t.rotation[axis]
            t.set_rotation(axis, was + f32(was + radians(value)), f32(value.value) if was == 0.0 else None)
        elif name in ('withScale', 'multiplyScale') and numbers and len(values) in (2, 3):
            for axis, value in self.by_axis(values, node):
                t.scale[axis] = f32(f32(value.value) * (1.0 if name == 'withScale' else t.scale[axis]))
        else:
            raise Cannot(f'`.{name}` of a transformation, called like this, is nothing this knows', node)
        return t

    @staticmethod
    def by_axis(values, node):
        """`(axis, value)` as one pair, `(x, y, z)` as three."""
        if len(values) == 3:
            return list(enumerate(values))
        if values[0].type != 'int' or values[0].value not in (0, 1, 2):
            raise Cannot('no axis this knows', node)
        return [(values[0].value, values[1])]

    def transformed_part(self, t, node):
        """A transformation as `JsonPose` reads one: a position, a rotation in degrees, a scale, a visibility."""
        name = t.part.name
        if t.part.root and self.context.json_root_name:
            name = '__root'
        out = {'part': name}
        if any(t.position):
            out['position'] = [Num('float', v) for v in t.position]
        if any(t.rotation):
            out['rotation'] = [Num('float', self.degrees(t, axis, node)) for axis in range(3)]
        if t.scale != [1.0, 1.0, 1.0]:
            out['scale'] = [Num('float', v) for v in t.scale]
        if t.visibility is not None:
            out['isVisible'] = t.visibility
        return out

    def degrees(self, t, axis, node):
        """Degrees `JsonPose` turns back (`withRotationDegrees`) into this very rotation."""
        radians = t.rotation[axis]
        if radians == 0.0:
            return 0.0
        guess = t.degrees[axis] if t.degrees[axis] is not None else f32(radians * f32(RADIAN_IN_DEGREES))
        for steps in (0, 1, -1, 2, -2, 3, -3):
            degrees = f32_neighbour(guess, steps)
            if f32(degrees / f32(RADIAN_IN_DEGREES)) == radians:
                return degrees
        self.note('rotation_rounded', f'no number of degrees is exactly the rotation {radians} radians of '
                  f'`{t.part.name}`: {guess} is the nearest', node)
        return guess

    # Poses.
    def fn_registerPose(self, node, env):
        names = [name for name, _ in node.args]
        first = self.ev(node.args[0][1], env) if node.args and node.args[0][0] is None else None
        second = self.ev(node.args[1][1], env) if len(node.args) > 1 and node.args[1][0] is None else None
        if 'poseTypes' in names or isinstance(second, PoseSet):
            head = ['poseName', 'poseTypes']
        elif 'poseName' in names or isinstance(first, str):
            head = ['poseName', 'poseType']
        else:
            head = ['poseType']
        bound = self.bind(node, head + REGISTER_POSE)
        if 'poseTypes' in bound:
            types = self.ev(bound['poseTypes'], env)
            if not isinstance(types, PoseSet):
                raise Cannot('pose types that are no set of them', bound['poseTypes'])
            types = [t.name for t in types]
        else:
            one = self.ev(bound['poseType'], env)
            if not isinstance(one, PoseType):
                raise Cannot('a pose type that is none', bound['poseType'])
            types = [one.name]
        name = self.string(bound['poseName'], env) if 'poseName' in bound else types[0]
        previous, self.where = self.where, f'pose `{name}`'
        try:
            pose = self.pose(name, types, bound, env, node)
        finally:
            self.where = previous
        if pose is not None:
            if name in self.poses:
                self.note('pose_registered_twice', f'`{name}` is registered twice: the later pose takes the '
                          "earlier one's place among the poses, as in the class's map", node, f'pose `{name}`')
            self.poses[name] = pose
        return PoseRef(name)

    def pose(self, name, types, bound, env, node):
        pose = {'poseName': name, 'poseTypes': types}
        for key in ('transformTicks', 'transformToTicks'):
            ticks = self.number(bound, key, env, Num('int', 10))
            if ticks.type != 'int' or ticks.value < 0:
                raise Cannot(f'`{key}` is no count of ticks', node)
            pose[key] = ticks.value
        if 'condition' in bound:
            keys, molang, omitted = self.condition(bound['condition'], types)
            for kind, reason, choice in omitted:
                self.drop('pose', kind, reason, node, choice)
            if omitted:
                return None
            pose.update(keys)
            if molang:
                pose['condition'] = molang
        animations = []
        for element in self.elements(bound.get('animations'), env, 'animation'):
            try:
                animation = self.ev(element, env)
                if not isinstance(animation, Animation):
                    raise Cannot('not a pose animation this knows', element)
            except Cannot as cannot:
                self.drop('animation', cannot.kind, cannot.reason, element)
                continue
            if animation.condition is None:
                animations.append(animation.molang)
            else:
                animations.append({'animation': animation.molang, 'condition': animation.condition})
        pose['animations'] = animations
        quirks = []
        for element in self.elements(bound.get('quirks'), env, 'quirk'):
            try:
                quirk = self.ev(element, env)
                if not isinstance(quirk, Quirk):
                    raise Cannot('not a quirk this knows', element)
                quirks.append(quirk.molang)
            except Cannot as cannot:
                self.drop('quirk', cannot.kind, cannot.reason, cannot.node if cannot.node is not None else element)
        if quirks:
            pose['quirks'] = quirks
        parts = []
        for element in self.elements(bound.get('transformedParts'), env, 'transformed part'):
            try:
                t = self.ev(element, env)
                if not isinstance(t, Transformation):
                    raise Cannot('not a transformation of a part', element)
                parts.append(self.transformed_part(t, element))
            except Cannot as cannot:
                self.drop('transformed part', cannot.kind, cannot.reason, element)
        if parts:
            pose['transformedParts'] = parts
        if 'namedAnimations' in bound:
            try:
                named = self.ev(bound['namedAnimations'], env)
                if not isinstance(named, dict) or not all(isinstance(v, ExpressionLike) for v in named.values()):
                    raise Cannot('named animations that are no map of MoLang', bound['namedAnimations'])
                if named:
                    pose['namedAnimations'] = {key: value.source for key, value in named.items()}
            except Cannot as cannot:
                self.drop('named animations', cannot.kind, cannot.reason, bound['namedAnimations'])
        if 'onTransitionedInto' in bound:
            self.drop(
                'onTransitionedInto', 'on_transitioned_into',
                'what a pose does once the model has moved into it: a JSON pose has no such thing '
                '(`PoseAdapter` leaves `onTransitionedInto` empty)', bound['onTransitionedInto'])
        return pose

    def elements(self, node, env, what):
        """The expressions of an `arrayOf(...)`, each to be read on its own."""
        if node is None:
            return []
        if node.k == 'call' and node.fn.k == 'name' and node.fn.id in ('arrayOf', 'emptyArray') and node.lam is None:
            return [value for _, value in node.args]
        self.drop(f'{what}s', 'not_understood', 'not written as an `arrayOf(...)`', node)
        return []

    # Conditions.
    def condition(self, node, types):
        """A pose's condition as `PoseAdapter`'s keys and one MoLang condition, and why the pose is to be
        left out, when it is: each time the kind of reason, the reason and the choice made."""
        if node.k != 'lambda' or len(node.body) != 1:
            return {}, None, [(
                'condition_not_understood', 'a condition that is not one expression',
                'pose left out: nothing says whether a wild Pokémon out of battle meets it')]
        state = (node.params or ['it'])[0]
        atoms, stack = [], [node.body[0]]
        while stack:
            atom = stack.pop()
            if atom.k == 'binary' and atom.op == '&&':
                stack += [atom.r, atom.l]
            else:
                atoms.append(atom)
        keys, molang, omitted = {}, [], []
        for atom in atoms:
            kind, *rest = self.atom(atom, state)
            if kind == 'key':
                key, wanted = rest
                if keys.get(key, wanted) != wanted:
                    omitted.append(('condition_contradicts_itself', f'`{key}` is asked both ways',
                                    'pose left out: it never fits'))
                keys[key] = wanted
            elif kind == 'molang':
                molang.append(rest[0])
                self.note('condition_in_molang', rest[1] + f': written as the MoLang condition `{rest[0]}`', atom)
            else:
                reason, wanted = rest
                if wanted is False:
                    self.drop(
                        'condition', 'condition_' + kind, reason, atom,
                        'pose kept without this requirement: it asks for the absence of something a wild '
                        'Pokémon out of battle is not usually doing, so the pose is what it shows anyway')
                else:
                    omitted.append((
                        'condition_' + kind, f'its condition asks for `{self.snippet(atom)}`: {reason}',
                        'pose left out: a pose kept without that requirement would be picked where the class '
                        'picks another'))
        if len(molang) > 1:
            self.note('condition_joined', 'several MoLang requirements, each in brackets and joined by `&&`', node)
            molang = [f'({source})' for source in molang]
        waits = [key for key in ('isTouchingWater', 'isUnderWater', 'isInWaterOrRain', 'isDusk') if keys.get(key)]
        if waits and not omitted and ('PROFILE' in types or 'PORTRAIT' in types):
            self.note(
                'fits_without_an_entity',
                f'{", ".join(waits)}: true in a pose that the party and summary screens can ask for. Without an '
                'entity (a model drawn in a screen) the class says the condition fails and `PoseAdapter` says '
                'it holds', node)
        return keys, ' && '.join(molang) or None, omitted

    def atom(self, node, state):
        """One requirement of a condition: a key of `PoseAdapter` and the value it has to have, MoLang that
        says the same, or what nothing in the JSON says."""
        negated = False
        while node.k == 'unary' and node.op == '!':
            negated, node = not negated, node.x
        if node.k == 'member' and is_name(node.obj, state) and node.name in STATE_KEYS:
            return 'key', STATE_KEYS[node.name], not negated
        if node.k == 'binary' and node.op in ('==', '!=') and node.r.k in ('bool', 'null'):
            # `x == true` and `x != true` of something that is null without an entity.
            holds = (node.op == '==') != negated
            asked = self.of_entity(node.l, state)
            if asked is not None and node.r.k == 'bool' and node.r.value is True:
                name, args, pokemon = asked
                if name == 'isDusk()' and not pokemon:
                    return 'key', 'isDusk', holds
                if name == 'isBattling' and pokemon:
                    return 'key', 'isBattle', holds
                if name == 'isStandingOn()' and not pokemon:
                    blocks = self.ev(args[0][1], {}) if args else None
                    depth = self.ev(args[1][1], {}) if len(args) > 1 else Num('int', 2)
                    if isinstance(blocks, list) and blocks and all(isinstance(b, str) for b in blocks) and \
                            isinstance(depth, Num) and len(args) <= 2:
                        written = ', '.join([decimal(depth)] + [quoted(block, node) for block in blocks])
                        call = f'q.is_standing_on_blocks({written})'
                        return ('molang', call if holds else '!' + call,
                                '`Entity.isStandingOn`, which is what `q.is_standing_on_blocks` asks')
                if name == 'isFalling()' and pokemon:
                    return ('inexpressible',
                            '`PokemonEntity.isFalling()`: no key of `PoseAdapter` and no MoLang query asks it',
                            holds)
            if asked is not None and node.r.k == 'null' and asked[0] == 'ownerUUID' and asked[2]:
                # `(entity as? PokemonEntity)?.ownerUUID == null` holds without an entity too.
                wild = 'q.has_entity ? q.is_wild : 1.0' if holds else 'q.has_entity ? !q.is_wild : 0.0'
                return ('molang', wild,
                        '`ownerUUID == null`, which is what `q.is_wild` answers for a Pokémon entity')
        return 'not_understood', 'a requirement this does not know how `PoseAdapter` would be asked for', None

    @staticmethod
    def of_entity(node, state):
        """`state.getEntity()?.name` or `(state.getEntity() as? PokemonEntity)?.name`, called or not: the
        name (with `()` when called), the call's arguments, and whether the entity was cast to a Pokémon's."""
        args = None
        if node.k == 'call' and node.lam is None:
            node, args = node.fn, node.args
        if node.k != 'member' or not node.safe:
            return None
        entity, pokemon = node.obj, False
        if entity.k == 'as' and entity.safe and entity.type == 'PokemonEntity':
            entity, pokemon = entity.x, True
        if entity.k == 'call' and not entity.args and entity.fn.k == 'member' and entity.fn.name == 'getEntity' \
                and is_name(entity.fn.obj, state):
            return node.name + ('()' if args is not None else ''), args or [], pokemon
        return None

    # The class itself.
    def read(self):
        for member in self.cls.members:
            try:
                self.class_member(member)
            except Cannot as cannot:
                if member.k == 'val':
                    self.env[member.name] = Poisoned(cannot)
                    if member.name in FRAME_FIELDS or 'override' in member.mods:
                        self.drop(f'`{member.name}`', cannot.kind, cannot.reason, member)
                else:
                    self.drop('member', cannot.kind, cannot.reason, member)
        register = [m for m in self.cls.members if m.k == 'fun' and m.name == 'registerPoses']
        if len(register) != 1:
            raise Cannot('no `registerPoses`')
        body = register[0].body
        env = {'#outer': self.env}
        for statement in body.body if body.k == 'block' else [body]:
            self.statement(statement, env)
        for name, provider in (('cry', self.cry), ('faint', self.faint)):
            if provider is not None:
                self.provider(name, *provider)

    def class_member(self, member):
        if member.k == 'fun':
            if member.name == 'registerPoses':
                return
            if member.name == 'getFaintAnimation' and len(member.params) == 1:
                self.faint = (member.body, member.params[0], member)
                return
            raise Cannot(f'the function `{member.name}` is nothing this knows', member)
        if member.k != 'val':
            raise Cannot('a member that is neither a value nor a function', member)
        name = member.name
        if member.init is None:
            if 'lateinit' in member.mods:
                self.env[name] = Unassigned(name)
                return
            raise Cannot('a value without a value', member)
        if name == 'cryAnimation':
            value = self.ev(member.init, self.env)
            if not isinstance(value, Lambda):
                raise Cannot('a cry that is no `CryProvider { ... }`', member)
            self.cry = (value.node, None, member)
            return
        value = self.ev(member.init, self.env)
        if name == 'rootPart':
            if not isinstance(value, Part) or not value.root:
                raise Cannot('a root part that is no child of the root', member)
            self.root_bone = value.name
        elif name in PORTRAIT:
            if isinstance(value, Vec3):
                value = value.xyz
            elif not isinstance(value, Num):
                raise Cannot(f'`{name}` is neither a number nor a `Vec3`', member)
            self.portrait[name] = value
        elif 'override' in member.mods and name not in FRAME_FIELDS:
            raise Cannot(f'the model\'s own `{name}`, which this does not know what to make of', member)
        self.env[name] = value
        self.frame.fields[name] = value

    def statement(self, node, env):
        try:
            if node.k == 'val':
                try:
                    env[node.name] = self.ev(node.init, env)
                except Cannot as cannot:
                    env[node.name] = Poisoned(cannot)
                    # A pose is not to be lost in the value it is kept in.
                    if self.registers_a_pose(node.init):
                        raise
                return
            if node.k == 'assign' and node.op == '=':
                target = node.target
                if target.k == 'name':
                    self.env[target.id] = self.ev(node.value, env)
                    return
                if target.k == 'index' and len(target.args) == 1:
                    key = self.string(target.args[0], env)
                    if is_name(target.obj, 'animations'):
                        value = self.ev(node.value, env)
                        if not isinstance(value, ExpressionLike):
                            raise Cannot('a named animation that is no MoLang', node)
                        self.named[key] = value.source
                        return
                    if is_name(target.obj, 'properties'):
                        self.properties[key] = self.string(node.value, env)
                        return
                    if target.obj.k == 'member' and target.obj.name == 'transitions':
                        pose = self.ev(target.obj.obj, env)
                        if isinstance(pose, PoseRef) and pose.name in self.poses and key in self.poses:
                            self.poses[pose.name].setdefault('transitions', {})[key] = self.transition(node.value, env)
                            return
                        raise Cannot('a transition between poses that are not both written', node)
                raise Cannot('an assignment this does not know what to make of', node)
            if node.k == 'call':
                self.ev(node, env)
                return
            raise Cannot('a statement this does not know what to make of', node)
        except Cannot as cannot:
            what = 'pose' if self.registers_a_pose(node) else 'statement'
            self.drop(what, cannot.kind, cannot.reason, node)

    @staticmethod
    def registers_a_pose(node):
        if node is not None and node.k in ('assign', 'val'):
            node = node.value if node.k == 'assign' else node.init
        return node is not None and node.k == 'call' and is_name(node.fn, 'registerPose')

    def active(self, value, node):
        """An active animation, as the MoLang that makes the same one."""
        if isinstance(value, Stateful):
            return f'q.bedrock_stateful({quoted(value.group, node)}, {quoted(value.name, node)})'
        if isinstance(value, Primary):
            inner = value.animation
            return f'q.bedrock_primary({quoted(inner.group, node)}, {quoted(inner.name, node)})'
        raise Cannot('not an animation this knows the MoLang of', node)

    def transition(self, node, env):
        """`{ _, _ -> bedrockStateful(...) }`: what plays from one pose to another."""
        if node.k != 'lambda' or len(node.body) != 1:
            raise Cannot('a transition that is not one expression', node)
        return self.active(self.ev(node.body[0], env), node)

    # Cries and faints: `PosableModel.getAnimation` asks the pose's named animations, then the model's, and
    # only then these two, which are code.
    def provider(self, name, body, state, member):
        self.where = 'model'
        if state is None:
            if body.k != 'lambda' or len(body.body) != 1:
                self.drop(name, 'not_understood', 'a provider that is not one expression', member)
                return
            state, body = (body.params or ['it'])[0], body.body[0]
        if name in self.named:
            self.note(
                'never_asked', f'the class\'s own `{name}` is never asked: the model has a named animation of '
                'that name, which comes first', member)
            return
        try:
            tree = self.decide(body, state)
        except Cannot as cannot:
            choice = None
            if cannot.kind == 'unassigned_pose':
                choice = 'left out: Cobblemon itself throws here (`UninitializedPropertyAccessException`) ' \
                    'whenever this animation is asked for'
            self.drop(name, cannot.kind, cannot.reason, member, choice)
            return
        try:
            # What each pose plays, None for nothing. A pose with a named animation of its own never asks.
            asked = {pose: self.settle(tree, pose, member) for pose, written in self.poses.items()
                     if name not in written.get('namedAnimations', {})}
        except Cannot as cannot:
            self.drop(name, cannot.kind, cannot.reason, member)
            return
        played = [source for source in asked.values() if source is not None]
        if not played:
            return
        if len(played) == len(asked):
            # The one most poses play is the model's; the others are the poses' own.
            common = max(sorted(set(played)), key=played.count)
            self.named[name] = common
            asked = {pose: source for pose, source in asked.items() if source != common}
        for pose, source in asked.items():
            if source is not None:
                self.poses[pose].setdefault('namedAnimations', {})[name] = source

    def decide(self, node, state):
        """A provider's body as a tree: ('play', MoLang or None), ('posed', names, then, else) or
        ('battle', then, else)."""
        if node.k == 'block' and len(node.body) == 1:
            node = node.body[0]
        if node.k == 'return' and node.value is not None:
            node = node.value
        if node.k == 'null':
            return ('play', None)
        if node.k == 'if':
            if node.els is None:
                raise Cannot('an `if` without an `else`', node)
            return self.branch(node.cond, state, self.decide(node.then, state), self.decide(node.els, state))
        if node.k == 'when' and node.subject is None:
            tree = None
            for conds, body in reversed(node.branches):
                if conds is None:
                    tree = self.decide(body, state)
                elif tree is None or len(conds) != 1:
                    raise Cannot('a `when` this does not know how to follow', node)
                else:
                    tree = self.branch(conds[0], state, self.decide(body, state), tree)
            if tree is None:
                raise Cannot('a `when` without branches', node)
            return tree
        return ('play', self.active(self.ev(node, {'#outer': self.env, state: Opaque('state')}), node))

    def branch(self, cond, state, then, els):
        negated = False
        while cond.k == 'unary' and cond.op == '!':
            negated, cond = not negated, cond.x
        if negated:
            then, els = els, then
        if cond.k == 'call' and cond.fn.k == 'member' and is_name(cond.fn.obj, state) and \
                cond.fn.name in ('isPosedIn', 'isNotPosedIn'):
            names = []
            for _, arg in cond.args:
                pose = self.ev(arg, self.env)
                if not isinstance(pose, PoseRef):
                    raise Cannot(f'`{self.snippet(arg)}` is read before any pose is put in it', arg,
                                 'unassigned_pose')
                names.append(pose.name)
            if cond.fn.name == 'isNotPosedIn':
                then, els = els, then
            return ('posed', names, then, els)
        if cond.k == 'member' and is_name(cond.obj, state) and cond.name == 'isBattling':
            return ('battle', then, els)
        if cond.k == 'binary' and cond.op == '==' and cond.r.k == 'bool' and cond.r.value is True:
            asked = self.of_entity(cond.l, state)
            if asked is not None and asked[0] == 'isBattling' and asked[2]:
                return ('battle', then, els)
        raise Cannot('a condition this does not know how to follow', cond)

    def settle(self, tree, pose, node):
        """What a provider plays in one pose, as MoLang: the battle it may ask about is `q.in_battle`."""
        if tree[0] == 'play':
            return tree[1]
        if tree[0] == 'posed':
            return self.settle(tree[2] if pose in tree[1] else tree[3], pose, node)
        then, els = self.settle(tree[1], pose, node), self.settle(tree[2], pose, node)
        if then == els:
            return then
        if then is None or els is None:
            raise Cannot('an animation in battle and none out of it, or the reverse: a named animation is '
                         'MoLang that has to give one', node, 'battle_or_nothing')
        if 'q.in_battle' in then or 'q.in_battle' in els:
            raise Cannot('a battle asked about twice', node)
        return f'q.in_battle ? {then} : {els}'


LOOK_OF_FRAME = [
    'invertX', 'invertY', 'disableX', 'disableY', 'pitchMultiplier', 'yawMultiplier', 'maxPitch', 'minPitch',
    'maxYaw', 'minYaw',
]
REGISTER_POSE = [
    'condition', 'transformTicks', 'transformToTicks', 'namedAnimations', 'onTransitionedInto', 'animations',
    'transformedParts', 'quirks',
]
WAVES = {
    'sineFunction': ['amplitude', 'period', 'phaseShift', 'verticalShift'],
    'cosineFunction': ['amplitude', 'period', 'phaseShift', 'verticalShift'],
    'triangleFunction': ['amplitude', 'period', 'phaseShift', 'verticalShift'],
}
# What a `PosableState` answers, and the key of `PoseAdapter` that asks the same of the entity.
STATE_KEYS = {
    'isBattling': 'isBattle',
    'isInWater': 'isTouchingWater',
    'isUnderWater': 'isUnderWater',
    'isInWaterOrRain': 'isInWaterOrRain',
}
PORTRAIT = ('portraitScale', 'portraitTranslation', 'profileScale', 'profileTranslation')
FRAME_FIELDS = (
    'rootPart', 'head', 'leftLeg', 'rightLeg', 'leftArm', 'rightArm', 'foreLeftLeg', 'foreRightLeg', 'hindLeftLeg',
    'hindRightLeg', 'leftWing', 'rightWing',
)

# ---------------------------------------------------------------------------------------------------------
# What this takes for granted of Cobblemon's own code, checked against the source it is given: each is a
# file and something that has to be in it, spaces aside. A Cobblemon that differs is not one this converts.
ASSUMPTIONS = [
    ('PosableModel.kt', 'transformTicks: Int = 10, transformToTicks: Int = 10,'),
    ('PosableModel.kt', 'secondsBetweenOccurrences: Pair<Float, Float> = 8F to 30F, loopTimes: IntRange = 1..1,'),
    ('PosableModel.kt', 'animationPrefix: String = "animation.$animationGroup"'),
    ('PosableModel.kt', '"cry" -> cryAnimation.invoke(state) "faint" -> getFaintAnimation(state)'),
    ('JsonPose.kt', 'val transformTicks = json.get("transformTicks")?.asInt ?: 10'),
    ('JsonPose.kt', 'val transformToTicks = json.get("transformToTicks")?.asInt'),
    ('JsonPose.kt', 'part.withPosition(position.x, position.y, position.z).withRotationDegrees(rotation.x, '
     'rotation.y, rotation.z).withScale(scale.x, scale.y, scale.z)'),
    ('JsonModelAdapter.kt', 'it.registerPartAndAllNamedChildren("__root", rootBone)'),
    ('PoseAdapter.kt', 'addCondition("isTouchingWater") { entity, expectedValue -> entity.isInWater == '
     'expectedValue }'),
    ('PoseAdapter.kt', 'addCondition("isInWaterOrRain") { entity, expectedValue -> entity.isInWaterOrRain == '
     'expectedValue }'),
    ('PoseAdapter.kt', 'addCondition("isUnderWater") { entity, expectedValue -> entity.isUnderWater == '
     'expectedValue }'),
    ('PoseAdapter.kt', 'addCondition("isDusk") { entity, expectedValue -> entity.isDusk() == expectedValue }'),
    ('PoseAdapter.kt', 'conditionsList.add { mustBeInBattle == it.isBattling }'),
    ('frame/HeadedFrame.kt', 'fun singleBoneLook(invertX: Boolean = false, invertY: Boolean = false, disableX: '
     'Boolean = false, disableY: Boolean = false, pitchMultiplier: Float? = null, yawMultiplier: Float? = null, '
     'maxPitch: Float? = null, minPitch: Float? = null, maxYaw: Float? = null, minYaw: Float? = null)'),
    ('animation/SingleBoneLookAnimation.kt', 'val bone: Bone?, val pitchMultiplier: Float = 1F, val yawMultiplier: '
     'Float = 1F, val maxPitch: Float = 70F, val minPitch: Float = -45F, val maxYaw: Float = 45F, val minYaw: '
     'Float = -45F,'),
    ('animation/SingleBoneLookAnimation.kt', 'pitchMultiplier = pitchMultiplier ?: if (disableX) 0F else if '
     '(invertX) -1F else 1F, yawMultiplier = yawMultiplier ?: if (disableY) 0F else if (invertY) -1F else 1F, '
     'maxPitch = maxPitch ?: 70F, minPitch = minPitch ?: -45F, maxYaw = maxYaw ?: 45F, minYaw = minYaw ?: -45F,'),
    ('animation/BipedWalkAnimation.kt', 'frame: BipedFrame, periodMultiplier: Float = 0.6662F, '
     'amplitudeMultiplier: Float = 1.4F'),
    ('animation/QuadrupedWalkAnimation.kt', 'frame: QuadrupedFrame, periodMultiplier: Float = 0.6662F, '
     'amplitudeMultiplier: Float = 1.4F'),
    ('animation/QuadrupedWalkAnimation.kt', 'legFrontLeft = frame.foreLeftLeg, legFrontRight = frame.foreRightLeg, '
     'legBackLeft = frame.hindLeftLeg, legBackRight = frame.hindRightLeg,'),
    ('animation/BimanualSwingAnimation.kt', 'frame: BimanualFrame, swingPeriodMultiplier: Float = 0.6662F, '
     'amplitudeMultiplier: Float = 1F'),
    ('animation/WingFlapIdleAnimation.kt', 'val timeVariable: (state: PosableState, limbSwing: Float, ageInTicks: '
     'Float) -> Float? = { state, _, _ -> state.animationSeconds },'),
    ('animation/PrimaryAnimation.kt', 'var curve: WaveFunction = { t -> if (t < 0.1) { t * 10 } else if (t < 0.9) '
     '{ 1F } else { 1F } }, /** Labels that can exist on a pose animation that will let it play even when this '
     'primary animation is in effect. */ val excludedLabels: Set<String> = emptySet(),'),
    ('wavefunction/WaveFunction.kt', 'fun sineFunction(amplitude: Float = 1F, period: Float = 1F, phaseShift: '
     'Float = 0F, verticalShift: Float = 0F): WaveFunction = { t -> sin(2*PI/period * (t - phaseShift)) * '
     'amplitude + verticalShift }'),
    ('pose/ModelPartTransformation.kt', 'return addRotation(axis, rotation[axis] + angle.toFloat().toRadians())'),
    ('pose/ModelPartTransformation.kt', 'if (visibility == true) { this.visibility = "true".asExpressionLike() } '
     'else { this.visibility = "0.0".asExpressionLike() }'),
    (f'/{COMMON}/util/math/geometry/AngleExtensions.kt', 'private const val RADIAN_IN_DEGREES = 57.2958f'),
    (f'/{COMMON}/util/MiscUtils.kt', 'val PosableState.isInWater: Boolean get() = getEntity()?.isInWater == true'),
    (f'/{COMMON}/util/MiscUtils.kt', 'val PosableState.isUnderWater: Boolean get() = getEntity()?.isUnderWater == '
     'true'),
    (f'/{COMMON}/util/MiscUtils.kt', 'val PosableState.isInWaterOrRain: Boolean get() = '
     'getEntity()?.isInWaterOrRain == true'),
    (f'/{COMMON}/client/ClientMoLangFunctions.kt', 'val minSeconds = params.getDoubleOrNull(2) ?: 8F val maxSeconds '
     '= params.getDoubleOrNull(3) ?: 30F val loopTimes = params.getDoubleOrNull(4)?.toInt() ?: 1'),
    (f'/{COMMON}/client/ClientMoLangFunctions.kt', 'val amplitude = params.getDoubleOrNull(0) ?: 0.9F val period = '
     'params.getDoubleOrNull(1) ?: 0.9F val verticalShift = params.getDoubleOrNull(2) ?: 0F val axis = '
     'params.getStringOrNull(3) ?: "y"'),
    (f'/{COMMON}/client/entity/PokemonClientDelegate.kt', '"in_battle" to java.util.function.Function { '
     'return@Function DoubleValue(currentEntity.isBattling) },'),
    (f'/{COMMON}/api/molang/function/PokemonEntityMoLangFunctions.kt', 'functions["is_wild"] = { '
     'DoubleValue(pokemonEntity.ownerUUID == null) }'),
    (f'/{COMMON}/api/molang/function/EntityMoLangFunctions.kt', 'if (entity.isStandingOn(blockStrings, depth)) '
     'DoubleValue.ONE else DoubleValue.ZERO'),
    (f'/{COMMON}/util/EntityExtensions.kt', 'fun Entity.isStandingOn(blocks: Set<String>, depth: Int = 2): Boolean {'),
]


def squeezed(text):
    return re.sub(r'\s+', '', text)


def check_assumptions(root):
    """Stops when the Cobblemon it is given is not the one this was written from."""
    failed, texts = [], {}
    for path, wanted in ASSUMPTIONS:
        file = root / path[1:] if path.startswith('/') else root / MODELS / path
        if file not in texts:
            texts[file] = squeezed(file.read_text(encoding='utf-8')) if file.is_file() else None
        if texts[file] is None:
            if f'{file}: not there' not in failed:
                failed.append(f'{file}: not there')
        elif squeezed(wanted) not in texts[file]:
            failed.append(f'{file}: no `{wanted}`')
    if failed:
        sys.exit('This Cobblemon is not the one this converter was written from:\n  ' + '\n  '.join(failed))


class Context:
    """What every class reads the same way: the names Kotlin imports, and how to write a root part."""

    def __init__(self, root, json_root_name):
        self.json_root_name = json_root_name
        pose_types = (root / COMMON / 'entity/PoseType.kt').read_text(encoding='utf-8')
        body = pose_types[pose_types.index('enum class PoseType'):]
        order = [name for name in re.findall(r'^\s*([A-Z_]+)\s*[,;]', body, re.M)]
        if order[:2] != ['STAND', 'WALK'] or len(order) != 14:
            sys.exit(f'PoseType.kt: its pose types read as {order}')
        members = {name: PoseType(name) for name in order}
        # An `EnumSet` iterates in the order of the enum, whatever order it was made in.
        for name, made in re.findall(r'val (\w+) = EnumSet\.(allOf\(PoseType::class\.java\)|of\([^)]*\))', body):
            listed = order if made.startswith('allOf') else re.findall(r'[A-Z_]+', made[3:])
            members[name] = PoseSet(PoseType(n) for n in order if n in listed)
        self.constants = dict(members)
        self.constants['PoseType'] = Namespace('PoseType', members)
        axes = {'X_AXIS': Num('int', 0), 'Y_AXIS': Num('int', 1), 'Z_AXIS': Num('int', 2)}
        self.constants.update(axes)
        self.constants['ModelPartTransformation'] = Namespace('ModelPartTransformation', axes)
        keys = (root / COMMON / 'util/DataKeys.kt').read_text(encoding='utf-8')
        self.constants['DataKeys'] = Namespace('DataKeys', dict(re.findall(r'const val (\w+) = "([^"$\\]*)"', keys)))


class Animations:
    """Cobblemon's own animation files, to tell when a class asks for an animation that is in none."""

    def __init__(self, root):
        # A group is a file's name, whatever folder it is in (`BedrockAnimationRepository`).
        self.files = {path.name[:-len('.animation.json')]: path
                      for path in sorted((root / ANIMATIONS).rglob('*.animation.json'))}
        self.texts = {}

    def lacks(self, group, name):
        """Why Cobblemon has no such animation, None when it has it."""
        if group not in self.files:
            return f'no animation group is called `{group}`'
        if group not in self.texts:
            self.texts[group] = self.files[group].read_text(encoding='utf-8-sig')
        if f'"animation.{group}.{name}"' not in self.texts[group]:
            return f'the group `{group}` has no `animation.{group}.{name}`'
        return None

    def check(self, model):
        """Notes the animations of a model's poses that Cobblemon lacks: it looks them up as it builds the
        model (`PosableModel.bedrock`), and throws."""
        for pose in model.poses.values():
            for animation in pose['animations']:
                source = animation if isinstance(animation, str) else animation['animation']
                asked = re.fullmatch(r"q\.bedrock\('([^']*)', '([^']*)'\)", source)
                why = self.lacks(*asked.groups()) if asked else None
                if why:
                    model.note(
                        'animation_cobblemon_lacks',
                        f'`{source}`: {why}. Cobblemon throws as it builds this model '
                        '(`BedrockAnimationRepository.getAnimation`), so it never draws with this poser',
                        where=f'pose `{pose["poseName"]}`')


# ---------------------------------------------------------------------------------------------------------
# Writing.

POSE_KEYS = [
    'animations', 'condition', 'isBattle', 'isDusk', 'isInWaterOrRain', 'isTouchingWater', 'isUnderWater',
    'namedAnimations', 'poseName', 'poseTypes', 'quirks', 'transformTicks', 'transformToTicks', 'transformedParts',
    'transitions',
]


def to_json(value, indent=0, keep_order=False, in_list=False):
    """JSON with an object's keys sorted, unless their order means something (`keep_order`: the poses'), a
    list of plain values on one line, as is a short object in a list, and no exponent in a number."""
    pad, inner = '  ' * indent, '  ' * (indent + 1)
    plain = lambda item: not isinstance(item, (dict, list))
    if isinstance(value, dict):
        if not value:
            return '{}'
        keys = list(value) if keep_order else sorted(value)
        if in_list and all(plain(v) or isinstance(v, list) and all(map(plain, v)) for v in value.values()):
            line = '{' + ', '.join(f'{json.dumps(key)}: {to_json(value[key])}' for key in keys) + '}'
            if '\n' not in line and len(line) + len(pad) <= 110:
                return line
        lines = [f'{inner}{json.dumps(key)}: {to_json(value[key], indent + 1, key == "poses")}' for key in keys]
        return '{\n' + ',\n'.join(lines) + '\n' + pad + '}'
    if isinstance(value, list):
        if all(map(plain, value)):
            line = '[' + ', '.join(to_json(item) for item in value) + ']'
            if len(line) + len(inner) <= 110:
                return line
        items = [inner + to_json(item, indent + 1, in_list=True) for item in value]
        return '[\n' + ',\n'.join(items) + '\n' + pad + ']'
    if isinstance(value, (bool, str)) or value is None:
        return json.dumps(value)
    return decimal(value)


def poser_json(model):
    out = dict(model.portrait)
    if model.root_bone is not None:
        out['rootBone'] = model.root_bone
    if model.named:
        out['animations'] = model.named
    if model.properties:
        out['properties'] = model.properties
    for pose in model.poses.values():
        unknown = set(pose) - set(POSE_KEYS)
        if unknown:
            raise AssertionError(f'a pose with {unknown}')
    out['poses'] = model.poses
    return to_json(out) + '\n'


NOTICE = """# Posers Cobblemon builds in code

Every `*.json` file of this folder, and `conversion-report.json`, is generated by
`{tool}` from the Kotlin model classes of Cobblemon 1.8.1
(`common/src/main/kotlin/com/cobblemon/mod/common/client/render/models/blockbench/pokemon/gen*/*Model.kt`,
https://gitlab.com/cable-mc/cobblemon), which are under the Mozilla Public License 2.0. They are that
code in another form, and are under the Mozilla Public License 2.0 too: https://mozilla.org/MPL/2.0/.

Each file is one poser `VaryingModelRepository.registerInBuiltPosers` registers, under the name it has
there, written in the format of Cobblemon's own poser files (`bedrock/pokemon/posers`). A class whose name
Cobblemon also has a poser file for has none here: Cobblemon loads the file over the class. Do not edit
these files: change the tool and run it again.

    python {tool} <cobblemon source root> <this folder>

What the format cannot say was left out, and `conversion-report.json` lists it, poser by poser, with the
Kotlin it came from.

One name in these files is not the one Cobblemon's own loader of poser files would know: a transformed
part that is the model's root part is written under its bone's name (`anorith`), as the classes register
it, where that loader registers the root part as `__root`. The tool's `--json-root-name` writes `__root`.
"""

MODULE = """// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.

// @generated by {tool}; do not edit.

//! The posers Cobblemon 1.8.1 builds in code (`VaryingModelRepository.registerInBuiltPosers`), as poser
//! files: what `{tool}` made of the Kotlin class behind each name, in the
//! format [`crate::poser::Poser::parse`] reads. The `conversion-report.json` beside those files lists what
//! that format could not say. A class Cobblemon has a poser file of the same name for is not here: the
//! file is the poser.

/// Every such poser by name, in the order of the names. One to a line, as the tool writes them.
#[rustfmt::skip]
const POSERS: [(&str, &str); {count}] = [
{entries}
];

/// The poser file of a poser Cobblemon builds in code, by its name without a namespace (`gengar` for
/// `cobblemon:gengar`). None for any other poser.
pub fn coded_poser(name: &str) -> Option<&'static str> {{
    POSERS
        .binary_search_by(|(known, _)| (*known).cmp(name))
        .ok()
        .map(|index| POSERS[index].1)
}}

/// The names of the posers Cobblemon builds in code, sorted.
pub fn names() -> impl Iterator<Item = &'static str> {{
    POSERS.iter().map(|(name, _)| *name)
}}
"""


def write(path, text):
    """Writes with LF line ends on every system, and only when something changed."""
    data = text.encode('utf-8')
    if not path.is_file() or path.read_bytes() != data:
        path.write_bytes(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('source', type=Path, help="Cobblemon 1.8.1's source root (the folder `common` is in)")
    parser.add_argument('output', type=Path, help='the folder the poser files go in')
    parser.add_argument('--module', type=Path, help='the Rust module to write (default: ../../src/coded_posers.rs '
                        'from the output folder)')
    parser.add_argument('--json-root-name', action='store_true', help='write a transformed root part as `__root`, '
                        "the name Cobblemon's JSON loader gives it, not as the bone's own name")
    args = parser.parse_args()
    root, out = args.source, args.output
    check_assumptions(root)
    context = Context(root, args.json_root_name)

    # Every model class, by name.
    classes = {}
    for path in sorted((root / MODELS / 'pokemon').glob('gen*/*.kt')):
        source = path.read_text(encoding='utf-8')
        relative = path.relative_to(root).as_posix()
        try:
            for cls in parse_classes(source):
                classes[cls.name] = (source, relative, cls, None)
        except ParseError as error:
            line = source.count('\n', 0, error.pos) + 1
            classes[path.stem] = (source, relative, None, f'line {line}: {error}')

    # The names they are registered under.
    repository = (root / MODELS / 'repository/VaryingModelRepository.kt').read_text(encoding='utf-8')
    registered = re.findall(r'^\s*inbuilt\("([^"]+)",\s*::\s*(\w+)\)', repository, re.M)
    # A name registered twice is the later class's (`posers[name] = ...`); 1.8.1 has `krabby` twice, for
    # the same class.
    names = {name: cls for name, cls in registered if cls in classes}
    twice = sorted({name for name, cls in registered if cls in classes and names[name] != cls})
    if twice:
        sys.exit(f'VaryingModelRepository.kt registers these names for two classes: {twice}')
    files_of_cobblemon = {}
    for path in sorted((root / POSERS).rglob('*.json')):
        files_of_cobblemon[path.stem] = path.relative_to(root).as_posix()
    animations = Animations(root)

    report, written, reasons = {}, {}, {}
    for name in sorted(names):
        source, relative, cls, error = classes[names[name]]
        entry = {'class': names[name], 'file': relative, 'dropped': [], 'notes': []}
        report[name] = entry
        if cls is None:
            entry['status'] = 'not converted'
            entry['error'] = f'the Kotlin does not read: {error}'
            continue
        model = Model(source, cls, context)
        try:
            model.read()
            if not model.poses:
                raise Cannot('no pose is left')
            text = poser_json(model)
            animations.check(model)
        except Cannot as cannot:
            entry['status'] = 'not converted'
            entry['error'] = cannot.reason
            entry['kotlin'] = model.snippet(cannot.node)
        else:
            entry['status'] = 'partial' if model.dropped else 'complete'
            # `registerJsonPosers` runs after `registerInBuiltPosers` and puts a file's poser where the
            # class of the same name was: such a class is never the poser, and no file is written for it.
            if name in files_of_cobblemon:
                entry['converts'] = entry['status']
                entry['status'] = 'superseded'
                entry['superseded by'] = files_of_cobblemon[name]
            else:
                written[name] = text
        entry['dropped'], entry['notes'] = model.dropped, model.notes
        if name in written:
            for dropped in model.dropped:
                reasons[dropped['kind']] = reasons.get(dropped['kind'], 0) + 1

    statuses = [entry['status'] for entry in report.values()]
    summary = {
        'model classes': len(classes),
        'posers': len(report),
        'written': len(written),
        'complete': statuses.count('complete'),
        'partial': statuses.count('partial'),
        'not converted': statuses.count('not converted'),
        "superseded by a poser file of Cobblemon's, not written": statuses.count('superseded'),
        'dropped from the written posers, by kind': reasons,
        'written posers with something dropped, by kind': {
            kind: sum(1 for name in written if any(d['kind'] == kind for d in report[name]['dropped']))
            for kind in reasons
        },
        'classes no poser name is registered for': sorted(set(classes) - set(names.values())),
    }
    out.mkdir(parents=True, exist_ok=True)
    for path in sorted(out.glob('*.json')):
        if path.stem not in written and path.name != 'conversion-report.json':
            print(f'removed {path.name}: no poser of that name is made any more')
            path.unlink()
    for name, text in written.items():
        write(out / f'{name}.json', text)
    write(out / 'NOTICE.md', NOTICE.format(tool=TOOL))
    write(out / 'conversion-report.json', to_json({
        'about': f'What {TOOL} made of each poser Cobblemon 1.8.1 builds in code. `dropped` is what the '
        "JSON format could not say and so is not in the poser's file, with the Kotlin it came from and, where "
        'there was a choice to make, the choice; `notes` is what was written in a way worth knowing about. A '
        'poser is `complete` when nothing was dropped, `partial` when something was, and `superseded` when '
        'Cobblemon has a poser file of the same name, which it loads over the class: no file is written for '
        'that one, and `converts` says how it would have gone.',
        'summary': summary,
        'posers': report,
    }) + '\n')

    module = (args.module or out / '../../src/coded_posers.rs').resolve()
    if module.parent.is_dir():
        folder = Path(os.path.relpath(out.resolve(), module.parent)).as_posix()
        entries = '\n'.join(f'    ("{name}", include_str!("{folder}/{name}.json")),' for name in sorted(written))
        write(module, MODULE.format(tool=TOOL, folder=folder, count=len(written), entries=entries))
    else:
        print(f'no Rust module written: {module.parent} is no folder')

    print(f'{len(classes)} model classes, {len(report)} registered posers: {len(written)} written '
          f'({summary["complete"]} complete, {summary["partial"]} partial), {statuses.count("superseded")} '
          f"superseded by a poser file of Cobblemon's, {summary['not converted']} not converted")
    for kind, count in sorted(reasons.items(), key=lambda item: (-item[1], item[0])):
        posers = summary['written posers with something dropped, by kind'][kind]
        print(f'  dropped, {kind}: {count} in {posers} posers')
    for name, entry in report.items():
        if entry['status'] == 'not converted':
            print(f'  not converted: {name} ({entry["class"]}): {entry["error"]}')


if __name__ == '__main__':
    main()
