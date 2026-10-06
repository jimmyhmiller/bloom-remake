//! Token and CST node kinds (LANGUAGE §§2–3, ARCHITECTURE §13.2).
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum SyntaxKind {
    /// eof.
    EOF,
    /// error.
    ERROR,
    /// missing.
    MISSING,
    /// ident.
    IDENT,
    /// bang ident.
    BANG_IDENT,
    /// int lit.
    INT_LIT,
    /// float lit.
    FLOAT_LIT,
    /// duration lit.
    DURATION_LIT,
    /// mod lit.
    MOD_LIT,
    /// string lit.
    STRING_LIT,
    /// raw string lit.
    RAW_STRING_LIT,
    /// bytes lit.
    BYTES_LIT,
    /// field num.
    FIELD_NUM,
    /// whitespace.
    WHITESPACE,
    /// line comment.
    LINE_COMMENT,
    /// doc comment.
    DOC_COMMENT,
    /// inner doc comment.
    INNER_DOC_COMMENT,
    /// block comment.
    BLOCK_COMMENT,
    /// hash comment.
    HASH_COMMENT,
    /// as kw.
    AS_KW,
    /// bootstrap kw.
    BOOTSTRAP_KW,
    /// channel kw.
    CHANNEL_KW,
    /// choreography kw.
    CHOREOGRAPHY_KW,
    /// const kw.
    CONST_KW,
    /// delete kw.
    DELETE_KW,
    /// else kw.
    ELSE_KW,
    /// emit kw.
    EMIT_KW,
    /// enum kw.
    ENUM_KW,
    /// extern kw.
    EXTERN_KW,
    /// false kw.
    FALSE_KW,
    /// fn kw.
    FN_KW,
    /// for kw.
    FOR_KW,
    /// if kw.
    IF_KW,
    /// impl kw.
    IMPL_KW,
    /// import kw.
    IMPORT_KW,
    /// in kw.
    IN_KW,
    /// include kw.
    INCLUDE_KW,
    /// input kw.
    INPUT_KW,
    /// interpose kw.
    INTERPOSE_KW,
    /// invariant kw.
    INVARIANT_KW,
    /// lattice kw.
    LATTICE_KW,
    /// let kw.
    LET_KW,
    /// loopback kw.
    LOOPBACK_KW,
    /// match kw.
    MATCH_KW,
    /// migrate kw.
    MIGRATE_KW,
    /// module kw.
    MODULE_KW,
    /// next kw.
    NEXT_KW,
    /// not kw.
    NOT_KW,
    /// on kw.
    ON_KW,
    /// output kw.
    OUTPUT_KW,
    /// override kw.
    OVERRIDE_KW,
    /// param kw.
    PARAM_KW,
    /// program kw.
    PROGRAM_KW,
    /// protocol kw.
    PROTOCOL_KW,
    /// pub kw.
    PUB_KW,
    /// scratch kw.
    SCRATCH_KW,
    /// seal kw.
    SEAL_KW,
    /// self kw.
    SELF_KW,
    /// send kw.
    SEND_KW,
    /// spec kw.
    SPEC_KW,
    /// static kw.
    STATIC_KW,
    /// struct kw.
    STRUCT_KW,
    /// table kw.
    TABLE_KW,
    /// translate kw.
    TRANSLATE_KW,
    /// true kw.
    TRUE_KW,
    /// type kw.
    TYPE_KW,
    /// upsert kw.
    UPSERT_KW,
    /// use kw.
    USE_KW,
    /// view kw.
    VIEW_KW,
    /// where kw.
    WHERE_KW,
    /// while kw.
    WHILE_KW,
    /// inner attr.
    INNER_ATTR,
    /// attr start.
    ATTR_START,
    /// open range eq.
    OPEN_RANGE_EQ,
    /// range eq.
    RANGE_EQ,
    /// open range.
    OPEN_RANGE,
    /// colon2.
    COLON2,
    /// range.
    RANGE,
    /// arrow.
    ARROW,
    /// fat arrow.
    FAT_ARROW,
    /// eq2.
    EQ2,
    /// neq.
    NEQ,
    /// le.
    LE,
    /// ge.
    GE,
    /// shl.
    SHL,
    /// shr.
    SHR,
    /// and2.
    AND2,
    /// or2.
    OR2,
    /// pow.
    POW,
    /// concat.
    CONCAT,
    /// l paren.
    L_PAREN,
    /// r paren.
    R_PAREN,
    /// l brack.
    L_BRACK,
    /// r brack.
    R_BRACK,
    /// l curly.
    L_CURLY,
    /// r curly.
    R_CURLY,
    /// comma.
    COMMA,
    /// semi.
    SEMI,
    /// colon.
    COLON,
    /// dot.
    DOT,
    /// at.
    AT,
    /// eq.
    EQ,
    /// lt.
    LT,
    /// gt.
    GT,
    /// plus.
    PLUS,
    /// minus.
    MINUS,
    /// star.
    STAR,
    /// slash.
    SLASH,
    /// percent.
    PERCENT,
    /// amp.
    AMP,
    /// pipe.
    PIPE,
    /// caret.
    CARET,
    /// tilde.
    TILDE,
    /// bang.
    BANG,
    /// question mark (`e?`: early return of `None`).
    QUESTION,
    /// underscore.
    UNDERSCORE,
    /// sourcefile.
    SOURCEFILE,
    /// programheader.
    PROGRAMHEADER,
    /// attr.
    ATTR,
    /// innerattr.
    INNERATTR,
    /// useitem.
    USEITEM,
    /// usetree.
    USETREE,
    /// importitem.
    IMPORTITEM,
    /// includeitem.
    INCLUDEITEM,
    /// constitem.
    CONSTITEM,
    /// paramitem.
    PARAMITEM,
    /// typealias.
    TYPEALIAS,
    /// structitem.
    STRUCTITEM,
    /// enumitem.
    ENUMITEM,
    /// variant.
    VARIANT,
    /// fielddecl.
    FIELDDECL,
    /// generics.
    GENERICS,
    /// genericparam.
    GENERICPARAM,
    /// genericargs.
    GENERICARGS,
    /// genericarg.
    GENERICARG,
    /// type.
    TYPE,
    /// fnitem.
    FNITEM,
    /// fnsig.
    FNSIG,
    /// fnparam.
    FNPARAM,
    /// blockexpr.
    BLOCKEXPR,
    /// externitem.
    EXTERNITEM,
    /// paramlist.
    PARAMLIST,
    /// param.
    PARAM,
    /// implitem.
    IMPLITEM,
    /// latticetypeitem.
    LATTICETYPEITEM,
    /// aggregateitem.
    AGGREGATEITEM,
    /// aggmember.
    AGGMEMBER,
    /// serviceitem.
    SERVICEITEM,
    /// moduleitem.
    MODULEITEM,
    /// modparams.
    MODPARAMS,
    /// modparam.
    MODPARAM,
    /// protocolitem.
    PROTOCOLITEM,
    /// roleitem.
    ROLEITEM,
    /// atsection.
    ATSECTION,
    /// interposeitem.
    INTERPOSEITEM,
    /// blockitem.
    BLOCKITEM,
    /// overrideitem.
    OVERRIDEITEM,
    /// aclitem.
    ACLITEM,
    /// reldecl.
    RELDECL,
    /// coldecl.
    COLDECL,
    /// directionclause.
    DIRECTIONCLAUSE,
    /// keyclause.
    KEYCLAUSE,
    /// ttlclause.
    TTLCLAUSE,
    /// maxclause.
    MAXCLAUSE,
    /// rangeclause.
    RANGECLAUSE,
    /// resolveclause.
    RESOLVECLAUSE,
    /// partitionclause.
    PARTITIONCLAUSE,
    /// sealedbyclause.
    SEALEDBYCLAUSE,
    /// exactlyonceclause.
    EXACTLYONCECLAUSE,
    WHILECLAUSE,
    /// policy.
    POLICY,
    /// celldecl.
    CELLDECL,
    /// timerdecl.
    TIMERDECL,
    /// viewdecl.
    VIEWDECL,
    /// viewcol.
    VIEWCOL,
    /// handleritem.
    HANDLERITEM,
    /// block.
    BLOCK,
    /// verbstmt.
    VERBSTMT,
    /// ifstmt.
    IFSTMT,
    /// forstmt.
    FORSTMT,
    /// head.
    HEAD,
    /// bootstrapitem.
    BOOTSTRAPITEM,
    /// factitem.
    FACTITEM,
    /// invariantitem.
    INVARIANTITEM,
    /// body.
    BODY,
    /// notlit.
    NOTLIT,
    /// letlit.
    LETLIT,
    /// outerlit.
    OUTERLIT,
    /// insertedlit.
    INSERTEDLIT,
    /// deletedlit.
    DELETEDLIT,
    /// sealedlit.
    SEALEDLIT,
    /// finallit.
    FINALLIT,
    /// perlit.
    PERLIT,
    /// anylit.
    ANYLIT,
    /// foralllit.
    FORALLLIT,
    /// everlit.
    EVERLIT,
    /// sentlit.
    SENTLIT,
    /// quorumlit.
    QUORUMLIT,
    /// atomlit.
    ATOMLIT,
    /// fromsuffix.
    FROMSUFFIX,
    /// principalsuffix.
    PRINCIPALSUFFIX,
    /// weightsuffix.
    WEIGHTSUFFIX,
    /// atsuffix.
    ATSUFFIX,
    /// atticksuffix.
    ATTICKSUFFIX,
    /// literalexpr.
    LITERALEXPR,
    /// pathexpr.
    PATHEXPR,
    /// callexpr.
    CALLEXPR,
    /// methodcallexpr.
    METHODCALLEXPR,
    /// tryexpr (`e?`).
    TRYEXPR,
    /// bangcallexpr.
    BANGCALLEXPR,
    /// fieldexpr.
    FIELDEXPR,
    /// tupleindexexpr.
    TUPLEINDEXEXPR,
    /// indexexpr.
    INDEXEXPR,
    /// binaryexpr.
    BINARYEXPR,
    /// prefixexpr.
    PREFIXEXPR,
    /// castexpr.
    CASTEXPR,
    /// parenexpr.
    PARENEXPR,
    /// tupleexpr.
    TUPLEEXPR,
    /// vecexpr.
    VECEXPR,
    /// setexpr.
    SETEXPR,
    /// mapexpr.
    MAPEXPR,
    /// foldexpr.
    FOLDEXPR,
    /// ifexpr.
    IFEXPR,
    /// matchexpr.
    MATCHEXPR,
    /// matcharm.
    MATCHARM,
    /// structlitexpr.
    STRUCTLITEXPR,
    /// fieldinit.
    FIELDINIT,
    /// closureexpr.
    CLOSUREEXPR,
    /// wildcard.
    WILDCARD,
    /// selfexpr.
    SELFEXPR,
    /// arg.
    ARG,
    /// bangclause.
    BANGCLAUSE,
    /// orderkeys.
    ORDERKEYS,
    /// orderkey.
    ORDERKEY,
    /// migrateitem.
    MIGRATEITEM,
    /// translateitem.
    TRANSLATEITEM,
    /// snapshotitem.
    SNAPSHOTITEM,
    /// specitem.
    SPECITEM,
    /// nodesmember.
    NODESMEMBER,
    /// assignmember.
    ASSIGNMEMBER,
    /// faultsmember.
    FAULTSMEMBER,
    /// livenessmember.
    LIVENESSMEMBER,
    /// provemember.
    PROVEMEMBER,
    /// expectmember.
    EXPECTMEMBER,
    /// checkmember.
    CHECKMEMBER,
    /// optblock.
    OPTBLOCK,
    /// optfield.
    OPTFIELD,
    /// name.
    NAME,
    /// relpath.
    RELPATH,
    /// streamitem: `stream NAME: listen;` or `stream NAME: connect;` (FOREIGN-PROTOCOLS §1).
    STREAMITEM,
    FORMATITEM,
    FORMATPARAM,
    FORMATFIELD,
    FORMATCOND,
    FORMATDEFAULT,
    /// `f"`: the start of an interpolated string (LANGUAGE §2.4).
    FSTRING_START,
    /// A run of an interpolated string's text (escapes and `{{`, `}}` included).
    FSTRING_TEXT,
    /// A hole's format spec, after its `:` (`.2`).
    FSTRING_SPEC,
    /// The closing `"` of an interpolated string.
    FSTRING_END,
    /// fstringexpr.
    FSTRINGEXPR,
    /// fstringhole.
    FSTRINGHOLE,
}
impl SyntaxKind {
    /// All kinds, in their wire/discriminant order.
    pub const ALL: &'static [Self] = &[
        Self::EOF,
        Self::ERROR,
        Self::MISSING,
        Self::IDENT,
        Self::BANG_IDENT,
        Self::INT_LIT,
        Self::FLOAT_LIT,
        Self::DURATION_LIT,
        Self::MOD_LIT,
        Self::STRING_LIT,
        Self::RAW_STRING_LIT,
        Self::BYTES_LIT,
        Self::FIELD_NUM,
        Self::WHITESPACE,
        Self::LINE_COMMENT,
        Self::DOC_COMMENT,
        Self::INNER_DOC_COMMENT,
        Self::BLOCK_COMMENT,
        Self::HASH_COMMENT,
        Self::AS_KW,
        Self::BOOTSTRAP_KW,
        Self::CHANNEL_KW,
        Self::CHOREOGRAPHY_KW,
        Self::CONST_KW,
        Self::DELETE_KW,
        Self::ELSE_KW,
        Self::EMIT_KW,
        Self::ENUM_KW,
        Self::EXTERN_KW,
        Self::FALSE_KW,
        Self::FN_KW,
        Self::FOR_KW,
        Self::IF_KW,
        Self::IMPL_KW,
        Self::IMPORT_KW,
        Self::IN_KW,
        Self::INCLUDE_KW,
        Self::INPUT_KW,
        Self::INTERPOSE_KW,
        Self::INVARIANT_KW,
        Self::LATTICE_KW,
        Self::LET_KW,
        Self::LOOPBACK_KW,
        Self::MATCH_KW,
        Self::MIGRATE_KW,
        Self::MODULE_KW,
        Self::NEXT_KW,
        Self::NOT_KW,
        Self::ON_KW,
        Self::OUTPUT_KW,
        Self::OVERRIDE_KW,
        Self::PARAM_KW,
        Self::PROGRAM_KW,
        Self::PROTOCOL_KW,
        Self::PUB_KW,
        Self::SCRATCH_KW,
        Self::SEAL_KW,
        Self::SELF_KW,
        Self::SEND_KW,
        Self::SPEC_KW,
        Self::STATIC_KW,
        Self::STRUCT_KW,
        Self::TABLE_KW,
        Self::TRANSLATE_KW,
        Self::TRUE_KW,
        Self::TYPE_KW,
        Self::UPSERT_KW,
        Self::USE_KW,
        Self::VIEW_KW,
        Self::WHERE_KW,
        Self::WHILE_KW,
        Self::INNER_ATTR,
        Self::ATTR_START,
        Self::OPEN_RANGE_EQ,
        Self::RANGE_EQ,
        Self::OPEN_RANGE,
        Self::COLON2,
        Self::RANGE,
        Self::ARROW,
        Self::FAT_ARROW,
        Self::EQ2,
        Self::NEQ,
        Self::LE,
        Self::GE,
        Self::SHL,
        Self::SHR,
        Self::AND2,
        Self::OR2,
        Self::POW,
        Self::CONCAT,
        Self::L_PAREN,
        Self::R_PAREN,
        Self::L_BRACK,
        Self::R_BRACK,
        Self::L_CURLY,
        Self::R_CURLY,
        Self::COMMA,
        Self::SEMI,
        Self::COLON,
        Self::DOT,
        Self::AT,
        Self::EQ,
        Self::LT,
        Self::GT,
        Self::PLUS,
        Self::MINUS,
        Self::STAR,
        Self::SLASH,
        Self::PERCENT,
        Self::AMP,
        Self::PIPE,
        Self::CARET,
        Self::TILDE,
        Self::BANG,
        Self::QUESTION,
        Self::UNDERSCORE,
        Self::SOURCEFILE,
        Self::PROGRAMHEADER,
        Self::ATTR,
        Self::INNERATTR,
        Self::USEITEM,
        Self::USETREE,
        Self::IMPORTITEM,
        Self::INCLUDEITEM,
        Self::CONSTITEM,
        Self::PARAMITEM,
        Self::TYPEALIAS,
        Self::STRUCTITEM,
        Self::ENUMITEM,
        Self::VARIANT,
        Self::FIELDDECL,
        Self::GENERICS,
        Self::GENERICPARAM,
        Self::GENERICARGS,
        Self::GENERICARG,
        Self::TYPE,
        Self::FNITEM,
        Self::FNSIG,
        Self::FNPARAM,
        Self::BLOCKEXPR,
        Self::EXTERNITEM,
        Self::PARAMLIST,
        Self::PARAM,
        Self::IMPLITEM,
        Self::LATTICETYPEITEM,
        Self::AGGREGATEITEM,
        Self::AGGMEMBER,
        Self::SERVICEITEM,
        Self::MODULEITEM,
        Self::MODPARAMS,
        Self::MODPARAM,
        Self::PROTOCOLITEM,
        Self::ROLEITEM,
        Self::ATSECTION,
        Self::INTERPOSEITEM,
        Self::BLOCKITEM,
        Self::OVERRIDEITEM,
        Self::ACLITEM,
        Self::RELDECL,
        Self::COLDECL,
        Self::DIRECTIONCLAUSE,
        Self::KEYCLAUSE,
        Self::TTLCLAUSE,
        Self::MAXCLAUSE,
        Self::RANGECLAUSE,
        Self::RESOLVECLAUSE,
        Self::PARTITIONCLAUSE,
        Self::SEALEDBYCLAUSE,
        Self::EXACTLYONCECLAUSE,
        Self::WHILECLAUSE,
        Self::POLICY,
        Self::CELLDECL,
        Self::TIMERDECL,
        Self::VIEWDECL,
        Self::VIEWCOL,
        Self::HANDLERITEM,
        Self::BLOCK,
        Self::VERBSTMT,
        Self::IFSTMT,
        Self::FORSTMT,
        Self::HEAD,
        Self::BOOTSTRAPITEM,
        Self::FACTITEM,
        Self::INVARIANTITEM,
        Self::BODY,
        Self::NOTLIT,
        Self::LETLIT,
        Self::OUTERLIT,
        Self::INSERTEDLIT,
        Self::DELETEDLIT,
        Self::SEALEDLIT,
        Self::FINALLIT,
        Self::PERLIT,
        Self::ANYLIT,
        Self::FORALLLIT,
        Self::EVERLIT,
        Self::SENTLIT,
        Self::QUORUMLIT,
        Self::ATOMLIT,
        Self::FROMSUFFIX,
        Self::PRINCIPALSUFFIX,
        Self::WEIGHTSUFFIX,
        Self::ATSUFFIX,
        Self::ATTICKSUFFIX,
        Self::LITERALEXPR,
        Self::PATHEXPR,
        Self::CALLEXPR,
        Self::METHODCALLEXPR,
        Self::TRYEXPR,
        Self::BANGCALLEXPR,
        Self::FIELDEXPR,
        Self::TUPLEINDEXEXPR,
        Self::INDEXEXPR,
        Self::BINARYEXPR,
        Self::PREFIXEXPR,
        Self::CASTEXPR,
        Self::PARENEXPR,
        Self::TUPLEEXPR,
        Self::VECEXPR,
        Self::SETEXPR,
        Self::MAPEXPR,
        Self::FOLDEXPR,
        Self::IFEXPR,
        Self::MATCHEXPR,
        Self::MATCHARM,
        Self::STRUCTLITEXPR,
        Self::FIELDINIT,
        Self::CLOSUREEXPR,
        Self::WILDCARD,
        Self::SELFEXPR,
        Self::ARG,
        Self::BANGCLAUSE,
        Self::ORDERKEYS,
        Self::ORDERKEY,
        Self::MIGRATEITEM,
        Self::TRANSLATEITEM,
        Self::SNAPSHOTITEM,
        Self::SPECITEM,
        Self::NODESMEMBER,
        Self::ASSIGNMEMBER,
        Self::FAULTSMEMBER,
        Self::LIVENESSMEMBER,
        Self::PROVEMEMBER,
        Self::EXPECTMEMBER,
        Self::CHECKMEMBER,
        Self::OPTBLOCK,
        Self::OPTFIELD,
        Self::NAME,
        Self::RELPATH,
        Self::STREAMITEM,
        Self::FORMATITEM,
        Self::FORMATPARAM,
        Self::FORMATFIELD,
        Self::FORMATCOND,
        Self::FORMATDEFAULT,
        Self::FSTRING_START,
        Self::FSTRING_TEXT,
        Self::FSTRING_SPEC,
        Self::FSTRING_END,
        Self::FSTRINGEXPR,
        Self::FSTRINGHOLE,
    ];
    /// Whether this token is trivia.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::WHITESPACE
                | Self::LINE_COMMENT
                | Self::DOC_COMMENT
                | Self::INNER_DOC_COMMENT
                | Self::BLOCK_COMMENT
                | Self::HASH_COMMENT
        )
    }
    /// Whether this kind is a hard keyword.
    pub fn is_keyword(self) -> bool {
        self >= Self::AS_KW && self <= Self::WHILE_KW
    }
    /// A hard keyword, or an identifier used in field-name position.
    pub fn is_word(self) -> bool {
        self == Self::IDENT || self.is_keyword()
    }
    /// Keyword recognition; contextual words stay identifiers.
    pub fn keyword(text: &str) -> Option<Self> {
        match text {
            "as" => Some(Self::AS_KW),
            "bootstrap" => Some(Self::BOOTSTRAP_KW),
            "channel" => Some(Self::CHANNEL_KW),
            "choreography" => Some(Self::CHOREOGRAPHY_KW),
            "const" => Some(Self::CONST_KW),
            "delete" => Some(Self::DELETE_KW),
            "else" => Some(Self::ELSE_KW),
            "emit" => Some(Self::EMIT_KW),
            "enum" => Some(Self::ENUM_KW),
            "extern" => Some(Self::EXTERN_KW),
            "false" => Some(Self::FALSE_KW),
            "fn" => Some(Self::FN_KW),
            "for" => Some(Self::FOR_KW),
            "if" => Some(Self::IF_KW),
            "impl" => Some(Self::IMPL_KW),
            "import" => Some(Self::IMPORT_KW),
            "in" => Some(Self::IN_KW),
            "include" => Some(Self::INCLUDE_KW),
            "input" => Some(Self::INPUT_KW),
            "interpose" => Some(Self::INTERPOSE_KW),
            "invariant" => Some(Self::INVARIANT_KW),
            "lattice" => Some(Self::LATTICE_KW),
            "let" => Some(Self::LET_KW),
            "loopback" => Some(Self::LOOPBACK_KW),
            "match" => Some(Self::MATCH_KW),
            "migrate" => Some(Self::MIGRATE_KW),
            "module" => Some(Self::MODULE_KW),
            "next" => Some(Self::NEXT_KW),
            "not" => Some(Self::NOT_KW),
            "on" => Some(Self::ON_KW),
            "output" => Some(Self::OUTPUT_KW),
            "override" => Some(Self::OVERRIDE_KW),
            "param" => Some(Self::PARAM_KW),
            "program" => Some(Self::PROGRAM_KW),
            "protocol" => Some(Self::PROTOCOL_KW),
            "pub" => Some(Self::PUB_KW),
            "scratch" => Some(Self::SCRATCH_KW),
            "seal" => Some(Self::SEAL_KW),
            "self" => Some(Self::SELF_KW),
            "send" => Some(Self::SEND_KW),
            "spec" => Some(Self::SPEC_KW),
            "static" => Some(Self::STATIC_KW),
            "struct" => Some(Self::STRUCT_KW),
            "table" => Some(Self::TABLE_KW),
            "translate" => Some(Self::TRANSLATE_KW),
            "true" => Some(Self::TRUE_KW),
            "type" => Some(Self::TYPE_KW),
            "upsert" => Some(Self::UPSERT_KW),
            "use" => Some(Self::USE_KW),
            "view" => Some(Self::VIEW_KW),
            "where" => Some(Self::WHERE_KW),
            "while" => Some(Self::WHILE_KW),
            _ => None,
        }
    }
    /// Longest-match punctuation table.
    pub const PUNCT: &'static [(&'static str, Self)] = &[
        ("#![", Self::INNER_ATTR),
        ("#[", Self::ATTR_START),
        ("<..=", Self::OPEN_RANGE_EQ),
        ("..=", Self::RANGE_EQ),
        ("<..", Self::OPEN_RANGE),
        ("::", Self::COLON2),
        ("..", Self::RANGE),
        ("->", Self::ARROW),
        ("=>", Self::FAT_ARROW),
        ("==", Self::EQ2),
        ("!=", Self::NEQ),
        ("<=", Self::LE),
        (">=", Self::GE),
        ("<<", Self::SHL),
        (">>", Self::SHR),
        ("&&", Self::AND2),
        ("||", Self::OR2),
        ("**", Self::POW),
        ("++", Self::CONCAT),
        ("(", Self::L_PAREN),
        (")", Self::R_PAREN),
        ("[", Self::L_BRACK),
        ("]", Self::R_BRACK),
        ("{", Self::L_CURLY),
        ("}", Self::R_CURLY),
        (",", Self::COMMA),
        (";", Self::SEMI),
        (":", Self::COLON),
        (".", Self::DOT),
        ("@", Self::AT),
        ("=", Self::EQ),
        ("<", Self::LT),
        (">", Self::GT),
        ("+", Self::PLUS),
        ("-", Self::MINUS),
        ("*", Self::STAR),
        ("/", Self::SLASH),
        ("%", Self::PERCENT),
        ("&", Self::AMP),
        ("|", Self::PIPE),
        ("^", Self::CARET),
        ("~", Self::TILDE),
        ("!", Self::BANG),
        ("?", Self::QUESTION),
        ("_", Self::UNDERSCORE),
    ];
}
/// The Rowan language marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BlossomLanguage {}
impl rowan::Language for BlossomLanguage {
    type Kind = SyntaxKind;
    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        SyntaxKind::ALL
            .get(usize::from(raw.0))
            .copied()
            .unwrap_or(SyntaxKind::ERROR)
    }
    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}
/// A red CST node.
pub type SyntaxNode = rowan::SyntaxNode<BlossomLanguage>;
/// A red CST token.
pub type SyntaxToken = rowan::SyntaxToken<BlossomLanguage>;
