// GENERATED CODE - DO NOT MODIFY BY HAND

part of 'app_database.dart';

// ignore_for_file: type=lint
class $AssetsTable extends Assets with TableInfo<$AssetsTable, AssetRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $AssetsTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _contentHashMeta = const VerificationMeta(
    'contentHash',
  );
  @override
  late final GeneratedColumn<String> contentHash = GeneratedColumn<String>(
    'content_hash',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _byteSizeMeta = const VerificationMeta(
    'byteSize',
  );
  @override
  late final GeneratedColumn<int> byteSize = GeneratedColumn<int>(
    'byte_size',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _extMeta = const VerificationMeta('ext');
  @override
  late final GeneratedColumn<String> ext = GeneratedColumn<String>(
    'ext',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _kindMeta = const VerificationMeta('kind');
  @override
  late final GeneratedColumn<String> kind = GeneratedColumn<String>(
    'kind',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant('audio'),
  );
  static const VerificationMeta _localRelPathMeta = const VerificationMeta(
    'localRelPath',
  );
  @override
  late final GeneratedColumn<String> localRelPath = GeneratedColumn<String>(
    'local_rel_path',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _recordSyncStateMeta = const VerificationMeta(
    'recordSyncState',
  );
  @override
  late final GeneratedColumn<String> recordSyncState = GeneratedColumn<String>(
    'record_sync_state',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant('localOnly'),
  );
  static const VerificationMeta _bytesSyncStateMeta = const VerificationMeta(
    'bytesSyncState',
  );
  @override
  late final GeneratedColumn<String> bytesSyncState = GeneratedColumn<String>(
    'bytes_sync_state',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant('localOnly'),
  );
  static const VerificationMeta _remoteRefMeta = const VerificationMeta(
    'remoteRef',
  );
  @override
  late final GeneratedColumn<String> remoteRef = GeneratedColumn<String>(
    'remote_ref',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    contentHash,
    byteSize,
    ext,
    kind,
    localRelPath,
    recordSyncState,
    bytesSyncState,
    remoteRef,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'assets';
  @override
  VerificationContext validateIntegrity(
    Insertable<AssetRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('content_hash')) {
      context.handle(
        _contentHashMeta,
        contentHash.isAcceptableOrUnknown(
          data['content_hash']!,
          _contentHashMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_contentHashMeta);
    }
    if (data.containsKey('byte_size')) {
      context.handle(
        _byteSizeMeta,
        byteSize.isAcceptableOrUnknown(data['byte_size']!, _byteSizeMeta),
      );
    } else if (isInserting) {
      context.missing(_byteSizeMeta);
    }
    if (data.containsKey('ext')) {
      context.handle(
        _extMeta,
        ext.isAcceptableOrUnknown(data['ext']!, _extMeta),
      );
    }
    if (data.containsKey('kind')) {
      context.handle(
        _kindMeta,
        kind.isAcceptableOrUnknown(data['kind']!, _kindMeta),
      );
    }
    if (data.containsKey('local_rel_path')) {
      context.handle(
        _localRelPathMeta,
        localRelPath.isAcceptableOrUnknown(
          data['local_rel_path']!,
          _localRelPathMeta,
        ),
      );
    }
    if (data.containsKey('record_sync_state')) {
      context.handle(
        _recordSyncStateMeta,
        recordSyncState.isAcceptableOrUnknown(
          data['record_sync_state']!,
          _recordSyncStateMeta,
        ),
      );
    }
    if (data.containsKey('bytes_sync_state')) {
      context.handle(
        _bytesSyncStateMeta,
        bytesSyncState.isAcceptableOrUnknown(
          data['bytes_sync_state']!,
          _bytesSyncStateMeta,
        ),
      );
    }
    if (data.containsKey('remote_ref')) {
      context.handle(
        _remoteRefMeta,
        remoteRef.isAcceptableOrUnknown(data['remote_ref']!, _remoteRefMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  AssetRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return AssetRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      contentHash: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}content_hash'],
      )!,
      byteSize: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}byte_size'],
      )!,
      ext: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}ext'],
      )!,
      kind: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}kind'],
      )!,
      localRelPath: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}local_rel_path'],
      ),
      recordSyncState: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}record_sync_state'],
      )!,
      bytesSyncState: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}bytes_sync_state'],
      )!,
      remoteRef: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}remote_ref'],
      ),
    );
  }

  @override
  $AssetsTable createAlias(String alias) {
    return $AssetsTable(attachedDatabase, alias);
  }
}

class AssetRow extends DataClass implements Insertable<AssetRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String contentHash;
  final int byteSize;
  final String ext;
  final String kind;
  final String? localRelPath;
  final String recordSyncState;
  final String bytesSyncState;
  final String? remoteRef;
  const AssetRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.contentHash,
    required this.byteSize,
    required this.ext,
    required this.kind,
    this.localRelPath,
    required this.recordSyncState,
    required this.bytesSyncState,
    this.remoteRef,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['content_hash'] = Variable<String>(contentHash);
    map['byte_size'] = Variable<int>(byteSize);
    map['ext'] = Variable<String>(ext);
    map['kind'] = Variable<String>(kind);
    if (!nullToAbsent || localRelPath != null) {
      map['local_rel_path'] = Variable<String>(localRelPath);
    }
    map['record_sync_state'] = Variable<String>(recordSyncState);
    map['bytes_sync_state'] = Variable<String>(bytesSyncState);
    if (!nullToAbsent || remoteRef != null) {
      map['remote_ref'] = Variable<String>(remoteRef);
    }
    return map;
  }

  AssetsCompanion toCompanion(bool nullToAbsent) {
    return AssetsCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      contentHash: Value(contentHash),
      byteSize: Value(byteSize),
      ext: Value(ext),
      kind: Value(kind),
      localRelPath: localRelPath == null && nullToAbsent
          ? const Value.absent()
          : Value(localRelPath),
      recordSyncState: Value(recordSyncState),
      bytesSyncState: Value(bytesSyncState),
      remoteRef: remoteRef == null && nullToAbsent
          ? const Value.absent()
          : Value(remoteRef),
    );
  }

  factory AssetRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return AssetRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      contentHash: serializer.fromJson<String>(json['contentHash']),
      byteSize: serializer.fromJson<int>(json['byteSize']),
      ext: serializer.fromJson<String>(json['ext']),
      kind: serializer.fromJson<String>(json['kind']),
      localRelPath: serializer.fromJson<String?>(json['localRelPath']),
      recordSyncState: serializer.fromJson<String>(json['recordSyncState']),
      bytesSyncState: serializer.fromJson<String>(json['bytesSyncState']),
      remoteRef: serializer.fromJson<String?>(json['remoteRef']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'contentHash': serializer.toJson<String>(contentHash),
      'byteSize': serializer.toJson<int>(byteSize),
      'ext': serializer.toJson<String>(ext),
      'kind': serializer.toJson<String>(kind),
      'localRelPath': serializer.toJson<String?>(localRelPath),
      'recordSyncState': serializer.toJson<String>(recordSyncState),
      'bytesSyncState': serializer.toJson<String>(bytesSyncState),
      'remoteRef': serializer.toJson<String?>(remoteRef),
    };
  }

  AssetRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? contentHash,
    int? byteSize,
    String? ext,
    String? kind,
    Value<String?> localRelPath = const Value.absent(),
    String? recordSyncState,
    String? bytesSyncState,
    Value<String?> remoteRef = const Value.absent(),
  }) => AssetRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    contentHash: contentHash ?? this.contentHash,
    byteSize: byteSize ?? this.byteSize,
    ext: ext ?? this.ext,
    kind: kind ?? this.kind,
    localRelPath: localRelPath.present ? localRelPath.value : this.localRelPath,
    recordSyncState: recordSyncState ?? this.recordSyncState,
    bytesSyncState: bytesSyncState ?? this.bytesSyncState,
    remoteRef: remoteRef.present ? remoteRef.value : this.remoteRef,
  );
  AssetRow copyWithCompanion(AssetsCompanion data) {
    return AssetRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      contentHash: data.contentHash.present
          ? data.contentHash.value
          : this.contentHash,
      byteSize: data.byteSize.present ? data.byteSize.value : this.byteSize,
      ext: data.ext.present ? data.ext.value : this.ext,
      kind: data.kind.present ? data.kind.value : this.kind,
      localRelPath: data.localRelPath.present
          ? data.localRelPath.value
          : this.localRelPath,
      recordSyncState: data.recordSyncState.present
          ? data.recordSyncState.value
          : this.recordSyncState,
      bytesSyncState: data.bytesSyncState.present
          ? data.bytesSyncState.value
          : this.bytesSyncState,
      remoteRef: data.remoteRef.present ? data.remoteRef.value : this.remoteRef,
    );
  }

  @override
  String toString() {
    return (StringBuffer('AssetRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('contentHash: $contentHash, ')
          ..write('byteSize: $byteSize, ')
          ..write('ext: $ext, ')
          ..write('kind: $kind, ')
          ..write('localRelPath: $localRelPath, ')
          ..write('recordSyncState: $recordSyncState, ')
          ..write('bytesSyncState: $bytesSyncState, ')
          ..write('remoteRef: $remoteRef')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    contentHash,
    byteSize,
    ext,
    kind,
    localRelPath,
    recordSyncState,
    bytesSyncState,
    remoteRef,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is AssetRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.contentHash == this.contentHash &&
          other.byteSize == this.byteSize &&
          other.ext == this.ext &&
          other.kind == this.kind &&
          other.localRelPath == this.localRelPath &&
          other.recordSyncState == this.recordSyncState &&
          other.bytesSyncState == this.bytesSyncState &&
          other.remoteRef == this.remoteRef);
}

class AssetsCompanion extends UpdateCompanion<AssetRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> contentHash;
  final Value<int> byteSize;
  final Value<String> ext;
  final Value<String> kind;
  final Value<String?> localRelPath;
  final Value<String> recordSyncState;
  final Value<String> bytesSyncState;
  final Value<String?> remoteRef;
  final Value<int> rowid;
  const AssetsCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.contentHash = const Value.absent(),
    this.byteSize = const Value.absent(),
    this.ext = const Value.absent(),
    this.kind = const Value.absent(),
    this.localRelPath = const Value.absent(),
    this.recordSyncState = const Value.absent(),
    this.bytesSyncState = const Value.absent(),
    this.remoteRef = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  AssetsCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String contentHash,
    required int byteSize,
    this.ext = const Value.absent(),
    this.kind = const Value.absent(),
    this.localRelPath = const Value.absent(),
    this.recordSyncState = const Value.absent(),
    this.bytesSyncState = const Value.absent(),
    this.remoteRef = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       contentHash = Value(contentHash),
       byteSize = Value(byteSize);
  static Insertable<AssetRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? contentHash,
    Expression<int>? byteSize,
    Expression<String>? ext,
    Expression<String>? kind,
    Expression<String>? localRelPath,
    Expression<String>? recordSyncState,
    Expression<String>? bytesSyncState,
    Expression<String>? remoteRef,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (contentHash != null) 'content_hash': contentHash,
      if (byteSize != null) 'byte_size': byteSize,
      if (ext != null) 'ext': ext,
      if (kind != null) 'kind': kind,
      if (localRelPath != null) 'local_rel_path': localRelPath,
      if (recordSyncState != null) 'record_sync_state': recordSyncState,
      if (bytesSyncState != null) 'bytes_sync_state': bytesSyncState,
      if (remoteRef != null) 'remote_ref': remoteRef,
      if (rowid != null) 'rowid': rowid,
    });
  }

  AssetsCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? contentHash,
    Value<int>? byteSize,
    Value<String>? ext,
    Value<String>? kind,
    Value<String?>? localRelPath,
    Value<String>? recordSyncState,
    Value<String>? bytesSyncState,
    Value<String?>? remoteRef,
    Value<int>? rowid,
  }) {
    return AssetsCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      contentHash: contentHash ?? this.contentHash,
      byteSize: byteSize ?? this.byteSize,
      ext: ext ?? this.ext,
      kind: kind ?? this.kind,
      localRelPath: localRelPath ?? this.localRelPath,
      recordSyncState: recordSyncState ?? this.recordSyncState,
      bytesSyncState: bytesSyncState ?? this.bytesSyncState,
      remoteRef: remoteRef ?? this.remoteRef,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (contentHash.present) {
      map['content_hash'] = Variable<String>(contentHash.value);
    }
    if (byteSize.present) {
      map['byte_size'] = Variable<int>(byteSize.value);
    }
    if (ext.present) {
      map['ext'] = Variable<String>(ext.value);
    }
    if (kind.present) {
      map['kind'] = Variable<String>(kind.value);
    }
    if (localRelPath.present) {
      map['local_rel_path'] = Variable<String>(localRelPath.value);
    }
    if (recordSyncState.present) {
      map['record_sync_state'] = Variable<String>(recordSyncState.value);
    }
    if (bytesSyncState.present) {
      map['bytes_sync_state'] = Variable<String>(bytesSyncState.value);
    }
    if (remoteRef.present) {
      map['remote_ref'] = Variable<String>(remoteRef.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('AssetsCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('contentHash: $contentHash, ')
          ..write('byteSize: $byteSize, ')
          ..write('ext: $ext, ')
          ..write('kind: $kind, ')
          ..write('localRelPath: $localRelPath, ')
          ..write('recordSyncState: $recordSyncState, ')
          ..write('bytesSyncState: $bytesSyncState, ')
          ..write('remoteRef: $remoteRef, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $PracticesTable extends Practices
    with TableInfo<$PracticesTable, PracticeRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $PracticesTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _titleMeta = const VerificationMeta('title');
  @override
  late final GeneratedColumn<String> title = GeneratedColumn<String>(
    'title',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _timelineDurationMsMeta =
      const VerificationMeta('timelineDurationMs');
  @override
  late final GeneratedColumn<int> timelineDurationMs = GeneratedColumn<int>(
    'timeline_duration_ms',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  static const VerificationMeta _masterVolumeMeta = const VerificationMeta(
    'masterVolume',
  );
  @override
  late final GeneratedColumn<double> masterVolume = GeneratedColumn<double>(
    'master_volume',
    aliasedName,
    false,
    type: DriftSqlType.double,
    requiredDuringInsert: false,
    defaultValue: const Constant(1.0),
  );
  static const VerificationMeta _activeInterpretationIdMeta =
      const VerificationMeta('activeInterpretationId');
  @override
  late final GeneratedColumn<String> activeInterpretationId =
      GeneratedColumn<String>(
        'active_interpretation_id',
        aliasedName,
        true,
        type: DriftSqlType.string,
        requiredDuringInsert: false,
      );
  static const VerificationMeta _separationAssetIdMeta = const VerificationMeta(
    'separationAssetId',
  );
  @override
  late final GeneratedColumn<String> separationAssetId =
      GeneratedColumn<String>(
        'separation_asset_id',
        aliasedName,
        true,
        type: DriftSqlType.string,
        requiredDuringInsert: false,
        defaultConstraints: GeneratedColumn.constraintIsAlways(
          'REFERENCES assets (id)',
        ),
      );
  static const VerificationMeta _reverbJsonMeta = const VerificationMeta(
    'reverbJson',
  );
  @override
  late final GeneratedColumn<String> reverbJson = GeneratedColumn<String>(
    'reverb_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _lyricsJsonMeta = const VerificationMeta(
    'lyricsJson',
  );
  @override
  late final GeneratedColumn<String> lyricsJson = GeneratedColumn<String>(
    'lyrics_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    title,
    timelineDurationMs,
    masterVolume,
    activeInterpretationId,
    separationAssetId,
    reverbJson,
    lyricsJson,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'practices';
  @override
  VerificationContext validateIntegrity(
    Insertable<PracticeRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('title')) {
      context.handle(
        _titleMeta,
        title.isAcceptableOrUnknown(data['title']!, _titleMeta),
      );
    }
    if (data.containsKey('timeline_duration_ms')) {
      context.handle(
        _timelineDurationMsMeta,
        timelineDurationMs.isAcceptableOrUnknown(
          data['timeline_duration_ms']!,
          _timelineDurationMsMeta,
        ),
      );
    }
    if (data.containsKey('master_volume')) {
      context.handle(
        _masterVolumeMeta,
        masterVolume.isAcceptableOrUnknown(
          data['master_volume']!,
          _masterVolumeMeta,
        ),
      );
    }
    if (data.containsKey('active_interpretation_id')) {
      context.handle(
        _activeInterpretationIdMeta,
        activeInterpretationId.isAcceptableOrUnknown(
          data['active_interpretation_id']!,
          _activeInterpretationIdMeta,
        ),
      );
    }
    if (data.containsKey('separation_asset_id')) {
      context.handle(
        _separationAssetIdMeta,
        separationAssetId.isAcceptableOrUnknown(
          data['separation_asset_id']!,
          _separationAssetIdMeta,
        ),
      );
    }
    if (data.containsKey('reverb_json')) {
      context.handle(
        _reverbJsonMeta,
        reverbJson.isAcceptableOrUnknown(data['reverb_json']!, _reverbJsonMeta),
      );
    }
    if (data.containsKey('lyrics_json')) {
      context.handle(
        _lyricsJsonMeta,
        lyricsJson.isAcceptableOrUnknown(data['lyrics_json']!, _lyricsJsonMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  PracticeRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return PracticeRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      title: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}title'],
      )!,
      timelineDurationMs: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}timeline_duration_ms'],
      )!,
      masterVolume: attachedDatabase.typeMapping.read(
        DriftSqlType.double,
        data['${effectivePrefix}master_volume'],
      )!,
      activeInterpretationId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}active_interpretation_id'],
      ),
      separationAssetId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}separation_asset_id'],
      ),
      reverbJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}reverb_json'],
      ),
      lyricsJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}lyrics_json'],
      ),
    );
  }

  @override
  $PracticesTable createAlias(String alias) {
    return $PracticesTable(attachedDatabase, alias);
  }
}

class PracticeRow extends DataClass implements Insertable<PracticeRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String title;
  final int timelineDurationMs;
  final double masterVolume;

  /// Which interpretation is active. Logical nullable pointer (self-healed by
  /// the IntegrityChecker if it ever resolves to a tombstoned interpretation).
  final String? activeInterpretationId;

  /// The pre-separation full mix this practice's stems came from (an asset).
  final String? separationAssetId;
  final String? reverbJson;
  final String? lyricsJson;
  const PracticeRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.title,
    required this.timelineDurationMs,
    required this.masterVolume,
    this.activeInterpretationId,
    this.separationAssetId,
    this.reverbJson,
    this.lyricsJson,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['title'] = Variable<String>(title);
    map['timeline_duration_ms'] = Variable<int>(timelineDurationMs);
    map['master_volume'] = Variable<double>(masterVolume);
    if (!nullToAbsent || activeInterpretationId != null) {
      map['active_interpretation_id'] = Variable<String>(
        activeInterpretationId,
      );
    }
    if (!nullToAbsent || separationAssetId != null) {
      map['separation_asset_id'] = Variable<String>(separationAssetId);
    }
    if (!nullToAbsent || reverbJson != null) {
      map['reverb_json'] = Variable<String>(reverbJson);
    }
    if (!nullToAbsent || lyricsJson != null) {
      map['lyrics_json'] = Variable<String>(lyricsJson);
    }
    return map;
  }

  PracticesCompanion toCompanion(bool nullToAbsent) {
    return PracticesCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      title: Value(title),
      timelineDurationMs: Value(timelineDurationMs),
      masterVolume: Value(masterVolume),
      activeInterpretationId: activeInterpretationId == null && nullToAbsent
          ? const Value.absent()
          : Value(activeInterpretationId),
      separationAssetId: separationAssetId == null && nullToAbsent
          ? const Value.absent()
          : Value(separationAssetId),
      reverbJson: reverbJson == null && nullToAbsent
          ? const Value.absent()
          : Value(reverbJson),
      lyricsJson: lyricsJson == null && nullToAbsent
          ? const Value.absent()
          : Value(lyricsJson),
    );
  }

  factory PracticeRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return PracticeRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      title: serializer.fromJson<String>(json['title']),
      timelineDurationMs: serializer.fromJson<int>(json['timelineDurationMs']),
      masterVolume: serializer.fromJson<double>(json['masterVolume']),
      activeInterpretationId: serializer.fromJson<String?>(
        json['activeInterpretationId'],
      ),
      separationAssetId: serializer.fromJson<String?>(
        json['separationAssetId'],
      ),
      reverbJson: serializer.fromJson<String?>(json['reverbJson']),
      lyricsJson: serializer.fromJson<String?>(json['lyricsJson']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'title': serializer.toJson<String>(title),
      'timelineDurationMs': serializer.toJson<int>(timelineDurationMs),
      'masterVolume': serializer.toJson<double>(masterVolume),
      'activeInterpretationId': serializer.toJson<String?>(
        activeInterpretationId,
      ),
      'separationAssetId': serializer.toJson<String?>(separationAssetId),
      'reverbJson': serializer.toJson<String?>(reverbJson),
      'lyricsJson': serializer.toJson<String?>(lyricsJson),
    };
  }

  PracticeRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? title,
    int? timelineDurationMs,
    double? masterVolume,
    Value<String?> activeInterpretationId = const Value.absent(),
    Value<String?> separationAssetId = const Value.absent(),
    Value<String?> reverbJson = const Value.absent(),
    Value<String?> lyricsJson = const Value.absent(),
  }) => PracticeRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    title: title ?? this.title,
    timelineDurationMs: timelineDurationMs ?? this.timelineDurationMs,
    masterVolume: masterVolume ?? this.masterVolume,
    activeInterpretationId: activeInterpretationId.present
        ? activeInterpretationId.value
        : this.activeInterpretationId,
    separationAssetId: separationAssetId.present
        ? separationAssetId.value
        : this.separationAssetId,
    reverbJson: reverbJson.present ? reverbJson.value : this.reverbJson,
    lyricsJson: lyricsJson.present ? lyricsJson.value : this.lyricsJson,
  );
  PracticeRow copyWithCompanion(PracticesCompanion data) {
    return PracticeRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      title: data.title.present ? data.title.value : this.title,
      timelineDurationMs: data.timelineDurationMs.present
          ? data.timelineDurationMs.value
          : this.timelineDurationMs,
      masterVolume: data.masterVolume.present
          ? data.masterVolume.value
          : this.masterVolume,
      activeInterpretationId: data.activeInterpretationId.present
          ? data.activeInterpretationId.value
          : this.activeInterpretationId,
      separationAssetId: data.separationAssetId.present
          ? data.separationAssetId.value
          : this.separationAssetId,
      reverbJson: data.reverbJson.present
          ? data.reverbJson.value
          : this.reverbJson,
      lyricsJson: data.lyricsJson.present
          ? data.lyricsJson.value
          : this.lyricsJson,
    );
  }

  @override
  String toString() {
    return (StringBuffer('PracticeRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('title: $title, ')
          ..write('timelineDurationMs: $timelineDurationMs, ')
          ..write('masterVolume: $masterVolume, ')
          ..write('activeInterpretationId: $activeInterpretationId, ')
          ..write('separationAssetId: $separationAssetId, ')
          ..write('reverbJson: $reverbJson, ')
          ..write('lyricsJson: $lyricsJson')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    title,
    timelineDurationMs,
    masterVolume,
    activeInterpretationId,
    separationAssetId,
    reverbJson,
    lyricsJson,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is PracticeRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.title == this.title &&
          other.timelineDurationMs == this.timelineDurationMs &&
          other.masterVolume == this.masterVolume &&
          other.activeInterpretationId == this.activeInterpretationId &&
          other.separationAssetId == this.separationAssetId &&
          other.reverbJson == this.reverbJson &&
          other.lyricsJson == this.lyricsJson);
}

class PracticesCompanion extends UpdateCompanion<PracticeRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> title;
  final Value<int> timelineDurationMs;
  final Value<double> masterVolume;
  final Value<String?> activeInterpretationId;
  final Value<String?> separationAssetId;
  final Value<String?> reverbJson;
  final Value<String?> lyricsJson;
  final Value<int> rowid;
  const PracticesCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.title = const Value.absent(),
    this.timelineDurationMs = const Value.absent(),
    this.masterVolume = const Value.absent(),
    this.activeInterpretationId = const Value.absent(),
    this.separationAssetId = const Value.absent(),
    this.reverbJson = const Value.absent(),
    this.lyricsJson = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  PracticesCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    this.title = const Value.absent(),
    this.timelineDurationMs = const Value.absent(),
    this.masterVolume = const Value.absent(),
    this.activeInterpretationId = const Value.absent(),
    this.separationAssetId = const Value.absent(),
    this.reverbJson = const Value.absent(),
    this.lyricsJson = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked);
  static Insertable<PracticeRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? title,
    Expression<int>? timelineDurationMs,
    Expression<double>? masterVolume,
    Expression<String>? activeInterpretationId,
    Expression<String>? separationAssetId,
    Expression<String>? reverbJson,
    Expression<String>? lyricsJson,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (title != null) 'title': title,
      if (timelineDurationMs != null)
        'timeline_duration_ms': timelineDurationMs,
      if (masterVolume != null) 'master_volume': masterVolume,
      if (activeInterpretationId != null)
        'active_interpretation_id': activeInterpretationId,
      if (separationAssetId != null) 'separation_asset_id': separationAssetId,
      if (reverbJson != null) 'reverb_json': reverbJson,
      if (lyricsJson != null) 'lyrics_json': lyricsJson,
      if (rowid != null) 'rowid': rowid,
    });
  }

  PracticesCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? title,
    Value<int>? timelineDurationMs,
    Value<double>? masterVolume,
    Value<String?>? activeInterpretationId,
    Value<String?>? separationAssetId,
    Value<String?>? reverbJson,
    Value<String?>? lyricsJson,
    Value<int>? rowid,
  }) {
    return PracticesCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      title: title ?? this.title,
      timelineDurationMs: timelineDurationMs ?? this.timelineDurationMs,
      masterVolume: masterVolume ?? this.masterVolume,
      activeInterpretationId:
          activeInterpretationId ?? this.activeInterpretationId,
      separationAssetId: separationAssetId ?? this.separationAssetId,
      reverbJson: reverbJson ?? this.reverbJson,
      lyricsJson: lyricsJson ?? this.lyricsJson,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (title.present) {
      map['title'] = Variable<String>(title.value);
    }
    if (timelineDurationMs.present) {
      map['timeline_duration_ms'] = Variable<int>(timelineDurationMs.value);
    }
    if (masterVolume.present) {
      map['master_volume'] = Variable<double>(masterVolume.value);
    }
    if (activeInterpretationId.present) {
      map['active_interpretation_id'] = Variable<String>(
        activeInterpretationId.value,
      );
    }
    if (separationAssetId.present) {
      map['separation_asset_id'] = Variable<String>(separationAssetId.value);
    }
    if (reverbJson.present) {
      map['reverb_json'] = Variable<String>(reverbJson.value);
    }
    if (lyricsJson.present) {
      map['lyrics_json'] = Variable<String>(lyricsJson.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('PracticesCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('title: $title, ')
          ..write('timelineDurationMs: $timelineDurationMs, ')
          ..write('masterVolume: $masterVolume, ')
          ..write('activeInterpretationId: $activeInterpretationId, ')
          ..write('separationAssetId: $separationAssetId, ')
          ..write('reverbJson: $reverbJson, ')
          ..write('lyricsJson: $lyricsJson, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $InterpretationsTable extends Interpretations
    with TableInfo<$InterpretationsTable, InterpretationRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $InterpretationsTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _practiceIdMeta = const VerificationMeta(
    'practiceId',
  );
  @override
  late final GeneratedColumn<String> practiceId = GeneratedColumn<String>(
    'practice_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES practices (id)',
    ),
  );
  static const VerificationMeta _nameMeta = const VerificationMeta('name');
  @override
  late final GeneratedColumn<String> name = GeneratedColumn<String>(
    'name',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _orderIndexMeta = const VerificationMeta(
    'orderIndex',
  );
  @override
  late final GeneratedColumn<int> orderIndex = GeneratedColumn<int>(
    'order_index',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    practiceId,
    name,
    orderIndex,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'interpretations';
  @override
  VerificationContext validateIntegrity(
    Insertable<InterpretationRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('practice_id')) {
      context.handle(
        _practiceIdMeta,
        practiceId.isAcceptableOrUnknown(data['practice_id']!, _practiceIdMeta),
      );
    } else if (isInserting) {
      context.missing(_practiceIdMeta);
    }
    if (data.containsKey('name')) {
      context.handle(
        _nameMeta,
        name.isAcceptableOrUnknown(data['name']!, _nameMeta),
      );
    }
    if (data.containsKey('order_index')) {
      context.handle(
        _orderIndexMeta,
        orderIndex.isAcceptableOrUnknown(data['order_index']!, _orderIndexMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  InterpretationRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return InterpretationRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      practiceId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}practice_id'],
      )!,
      name: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}name'],
      )!,
      orderIndex: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}order_index'],
      )!,
    );
  }

  @override
  $InterpretationsTable createAlias(String alias) {
    return $InterpretationsTable(attachedDatabase, alias);
  }
}

class InterpretationRow extends DataClass
    implements Insertable<InterpretationRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String practiceId;
  final String name;
  final int orderIndex;
  const InterpretationRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.practiceId,
    required this.name,
    required this.orderIndex,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['practice_id'] = Variable<String>(practiceId);
    map['name'] = Variable<String>(name);
    map['order_index'] = Variable<int>(orderIndex);
    return map;
  }

  InterpretationsCompanion toCompanion(bool nullToAbsent) {
    return InterpretationsCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      practiceId: Value(practiceId),
      name: Value(name),
      orderIndex: Value(orderIndex),
    );
  }

  factory InterpretationRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return InterpretationRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      practiceId: serializer.fromJson<String>(json['practiceId']),
      name: serializer.fromJson<String>(json['name']),
      orderIndex: serializer.fromJson<int>(json['orderIndex']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'practiceId': serializer.toJson<String>(practiceId),
      'name': serializer.toJson<String>(name),
      'orderIndex': serializer.toJson<int>(orderIndex),
    };
  }

  InterpretationRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? practiceId,
    String? name,
    int? orderIndex,
  }) => InterpretationRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    practiceId: practiceId ?? this.practiceId,
    name: name ?? this.name,
    orderIndex: orderIndex ?? this.orderIndex,
  );
  InterpretationRow copyWithCompanion(InterpretationsCompanion data) {
    return InterpretationRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      practiceId: data.practiceId.present
          ? data.practiceId.value
          : this.practiceId,
      name: data.name.present ? data.name.value : this.name,
      orderIndex: data.orderIndex.present
          ? data.orderIndex.value
          : this.orderIndex,
    );
  }

  @override
  String toString() {
    return (StringBuffer('InterpretationRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('practiceId: $practiceId, ')
          ..write('name: $name, ')
          ..write('orderIndex: $orderIndex')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    practiceId,
    name,
    orderIndex,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is InterpretationRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.practiceId == this.practiceId &&
          other.name == this.name &&
          other.orderIndex == this.orderIndex);
}

class InterpretationsCompanion extends UpdateCompanion<InterpretationRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> practiceId;
  final Value<String> name;
  final Value<int> orderIndex;
  final Value<int> rowid;
  const InterpretationsCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.practiceId = const Value.absent(),
    this.name = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  InterpretationsCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String practiceId,
    this.name = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       practiceId = Value(practiceId);
  static Insertable<InterpretationRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? practiceId,
    Expression<String>? name,
    Expression<int>? orderIndex,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (practiceId != null) 'practice_id': practiceId,
      if (name != null) 'name': name,
      if (orderIndex != null) 'order_index': orderIndex,
      if (rowid != null) 'rowid': rowid,
    });
  }

  InterpretationsCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? practiceId,
    Value<String>? name,
    Value<int>? orderIndex,
    Value<int>? rowid,
  }) {
    return InterpretationsCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      practiceId: practiceId ?? this.practiceId,
      name: name ?? this.name,
      orderIndex: orderIndex ?? this.orderIndex,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (practiceId.present) {
      map['practice_id'] = Variable<String>(practiceId.value);
    }
    if (name.present) {
      map['name'] = Variable<String>(name.value);
    }
    if (orderIndex.present) {
      map['order_index'] = Variable<int>(orderIndex.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('InterpretationsCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('practiceId: $practiceId, ')
          ..write('name: $name, ')
          ..write('orderIndex: $orderIndex, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $FxBusesTable extends FxBuses with TableInfo<$FxBusesTable, FxBusRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $FxBusesTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _practiceIdMeta = const VerificationMeta(
    'practiceId',
  );
  @override
  late final GeneratedColumn<String> practiceId = GeneratedColumn<String>(
    'practice_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES practices (id)',
    ),
  );
  static const VerificationMeta _nameMeta = const VerificationMeta('name');
  @override
  late final GeneratedColumn<String> name = GeneratedColumn<String>(
    'name',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _colorValueMeta = const VerificationMeta(
    'colorValue',
  );
  @override
  late final GeneratedColumn<int> colorValue = GeneratedColumn<int>(
    'color_value',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _volumeMeta = const VerificationMeta('volume');
  @override
  late final GeneratedColumn<double> volume = GeneratedColumn<double>(
    'volume',
    aliasedName,
    false,
    type: DriftSqlType.double,
    requiredDuringInsert: false,
    defaultValue: const Constant(1.0),
  );
  static const VerificationMeta _panMeta = const VerificationMeta('pan');
  @override
  late final GeneratedColumn<double> pan = GeneratedColumn<double>(
    'pan',
    aliasedName,
    false,
    type: DriftSqlType.double,
    requiredDuringInsert: false,
    defaultValue: const Constant(0.0),
  );
  static const VerificationMeta _mutedMeta = const VerificationMeta('muted');
  @override
  late final GeneratedColumn<bool> muted = GeneratedColumn<bool>(
    'muted',
    aliasedName,
    false,
    type: DriftSqlType.bool,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'CHECK ("muted" IN (0, 1))',
    ),
    defaultValue: const Constant(false),
  );
  static const VerificationMeta _soloedMeta = const VerificationMeta('soloed');
  @override
  late final GeneratedColumn<bool> soloed = GeneratedColumn<bool>(
    'soloed',
    aliasedName,
    false,
    type: DriftSqlType.bool,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'CHECK ("soloed" IN (0, 1))',
    ),
    defaultValue: const Constant(false),
  );
  static const VerificationMeta _outputBusIdMeta = const VerificationMeta(
    'outputBusId',
  );
  @override
  late final GeneratedColumn<String> outputBusId = GeneratedColumn<String>(
    'output_bus_id',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _fxJsonMeta = const VerificationMeta('fxJson');
  @override
  late final GeneratedColumn<String> fxJson = GeneratedColumn<String>(
    'fx_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _sendsJsonMeta = const VerificationMeta(
    'sendsJson',
  );
  @override
  late final GeneratedColumn<String> sendsJson = GeneratedColumn<String>(
    'sends_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _orderIndexMeta = const VerificationMeta(
    'orderIndex',
  );
  @override
  late final GeneratedColumn<int> orderIndex = GeneratedColumn<int>(
    'order_index',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    practiceId,
    name,
    colorValue,
    volume,
    pan,
    muted,
    soloed,
    outputBusId,
    fxJson,
    sendsJson,
    orderIndex,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'fx_buses';
  @override
  VerificationContext validateIntegrity(
    Insertable<FxBusRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('practice_id')) {
      context.handle(
        _practiceIdMeta,
        practiceId.isAcceptableOrUnknown(data['practice_id']!, _practiceIdMeta),
      );
    } else if (isInserting) {
      context.missing(_practiceIdMeta);
    }
    if (data.containsKey('name')) {
      context.handle(
        _nameMeta,
        name.isAcceptableOrUnknown(data['name']!, _nameMeta),
      );
    }
    if (data.containsKey('color_value')) {
      context.handle(
        _colorValueMeta,
        colorValue.isAcceptableOrUnknown(data['color_value']!, _colorValueMeta),
      );
    }
    if (data.containsKey('volume')) {
      context.handle(
        _volumeMeta,
        volume.isAcceptableOrUnknown(data['volume']!, _volumeMeta),
      );
    }
    if (data.containsKey('pan')) {
      context.handle(
        _panMeta,
        pan.isAcceptableOrUnknown(data['pan']!, _panMeta),
      );
    }
    if (data.containsKey('muted')) {
      context.handle(
        _mutedMeta,
        muted.isAcceptableOrUnknown(data['muted']!, _mutedMeta),
      );
    }
    if (data.containsKey('soloed')) {
      context.handle(
        _soloedMeta,
        soloed.isAcceptableOrUnknown(data['soloed']!, _soloedMeta),
      );
    }
    if (data.containsKey('output_bus_id')) {
      context.handle(
        _outputBusIdMeta,
        outputBusId.isAcceptableOrUnknown(
          data['output_bus_id']!,
          _outputBusIdMeta,
        ),
      );
    }
    if (data.containsKey('fx_json')) {
      context.handle(
        _fxJsonMeta,
        fxJson.isAcceptableOrUnknown(data['fx_json']!, _fxJsonMeta),
      );
    }
    if (data.containsKey('sends_json')) {
      context.handle(
        _sendsJsonMeta,
        sendsJson.isAcceptableOrUnknown(data['sends_json']!, _sendsJsonMeta),
      );
    }
    if (data.containsKey('order_index')) {
      context.handle(
        _orderIndexMeta,
        orderIndex.isAcceptableOrUnknown(data['order_index']!, _orderIndexMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  FxBusRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return FxBusRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      practiceId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}practice_id'],
      )!,
      name: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}name'],
      )!,
      colorValue: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}color_value'],
      ),
      volume: attachedDatabase.typeMapping.read(
        DriftSqlType.double,
        data['${effectivePrefix}volume'],
      )!,
      pan: attachedDatabase.typeMapping.read(
        DriftSqlType.double,
        data['${effectivePrefix}pan'],
      )!,
      muted: attachedDatabase.typeMapping.read(
        DriftSqlType.bool,
        data['${effectivePrefix}muted'],
      )!,
      soloed: attachedDatabase.typeMapping.read(
        DriftSqlType.bool,
        data['${effectivePrefix}soloed'],
      )!,
      outputBusId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}output_bus_id'],
      ),
      fxJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}fx_json'],
      ),
      sendsJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}sends_json'],
      ),
      orderIndex: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}order_index'],
      )!,
    );
  }

  @override
  $FxBusesTable createAlias(String alias) {
    return $FxBusesTable(attachedDatabase, alias);
  }
}

class FxBusRow extends DataClass implements Insertable<FxBusRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String practiceId;
  final String name;
  final int? colorValue;
  final double volume;
  final double pan;
  final bool muted;
  final bool soloed;
  final String? outputBusId;
  final String? fxJson;
  final String? sendsJson;
  final int orderIndex;
  const FxBusRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.practiceId,
    required this.name,
    this.colorValue,
    required this.volume,
    required this.pan,
    required this.muted,
    required this.soloed,
    this.outputBusId,
    this.fxJson,
    this.sendsJson,
    required this.orderIndex,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['practice_id'] = Variable<String>(practiceId);
    map['name'] = Variable<String>(name);
    if (!nullToAbsent || colorValue != null) {
      map['color_value'] = Variable<int>(colorValue);
    }
    map['volume'] = Variable<double>(volume);
    map['pan'] = Variable<double>(pan);
    map['muted'] = Variable<bool>(muted);
    map['soloed'] = Variable<bool>(soloed);
    if (!nullToAbsent || outputBusId != null) {
      map['output_bus_id'] = Variable<String>(outputBusId);
    }
    if (!nullToAbsent || fxJson != null) {
      map['fx_json'] = Variable<String>(fxJson);
    }
    if (!nullToAbsent || sendsJson != null) {
      map['sends_json'] = Variable<String>(sendsJson);
    }
    map['order_index'] = Variable<int>(orderIndex);
    return map;
  }

  FxBusesCompanion toCompanion(bool nullToAbsent) {
    return FxBusesCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      practiceId: Value(practiceId),
      name: Value(name),
      colorValue: colorValue == null && nullToAbsent
          ? const Value.absent()
          : Value(colorValue),
      volume: Value(volume),
      pan: Value(pan),
      muted: Value(muted),
      soloed: Value(soloed),
      outputBusId: outputBusId == null && nullToAbsent
          ? const Value.absent()
          : Value(outputBusId),
      fxJson: fxJson == null && nullToAbsent
          ? const Value.absent()
          : Value(fxJson),
      sendsJson: sendsJson == null && nullToAbsent
          ? const Value.absent()
          : Value(sendsJson),
      orderIndex: Value(orderIndex),
    );
  }

  factory FxBusRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return FxBusRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      practiceId: serializer.fromJson<String>(json['practiceId']),
      name: serializer.fromJson<String>(json['name']),
      colorValue: serializer.fromJson<int?>(json['colorValue']),
      volume: serializer.fromJson<double>(json['volume']),
      pan: serializer.fromJson<double>(json['pan']),
      muted: serializer.fromJson<bool>(json['muted']),
      soloed: serializer.fromJson<bool>(json['soloed']),
      outputBusId: serializer.fromJson<String?>(json['outputBusId']),
      fxJson: serializer.fromJson<String?>(json['fxJson']),
      sendsJson: serializer.fromJson<String?>(json['sendsJson']),
      orderIndex: serializer.fromJson<int>(json['orderIndex']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'practiceId': serializer.toJson<String>(practiceId),
      'name': serializer.toJson<String>(name),
      'colorValue': serializer.toJson<int?>(colorValue),
      'volume': serializer.toJson<double>(volume),
      'pan': serializer.toJson<double>(pan),
      'muted': serializer.toJson<bool>(muted),
      'soloed': serializer.toJson<bool>(soloed),
      'outputBusId': serializer.toJson<String?>(outputBusId),
      'fxJson': serializer.toJson<String?>(fxJson),
      'sendsJson': serializer.toJson<String?>(sendsJson),
      'orderIndex': serializer.toJson<int>(orderIndex),
    };
  }

  FxBusRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? practiceId,
    String? name,
    Value<int?> colorValue = const Value.absent(),
    double? volume,
    double? pan,
    bool? muted,
    bool? soloed,
    Value<String?> outputBusId = const Value.absent(),
    Value<String?> fxJson = const Value.absent(),
    Value<String?> sendsJson = const Value.absent(),
    int? orderIndex,
  }) => FxBusRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    practiceId: practiceId ?? this.practiceId,
    name: name ?? this.name,
    colorValue: colorValue.present ? colorValue.value : this.colorValue,
    volume: volume ?? this.volume,
    pan: pan ?? this.pan,
    muted: muted ?? this.muted,
    soloed: soloed ?? this.soloed,
    outputBusId: outputBusId.present ? outputBusId.value : this.outputBusId,
    fxJson: fxJson.present ? fxJson.value : this.fxJson,
    sendsJson: sendsJson.present ? sendsJson.value : this.sendsJson,
    orderIndex: orderIndex ?? this.orderIndex,
  );
  FxBusRow copyWithCompanion(FxBusesCompanion data) {
    return FxBusRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      practiceId: data.practiceId.present
          ? data.practiceId.value
          : this.practiceId,
      name: data.name.present ? data.name.value : this.name,
      colorValue: data.colorValue.present
          ? data.colorValue.value
          : this.colorValue,
      volume: data.volume.present ? data.volume.value : this.volume,
      pan: data.pan.present ? data.pan.value : this.pan,
      muted: data.muted.present ? data.muted.value : this.muted,
      soloed: data.soloed.present ? data.soloed.value : this.soloed,
      outputBusId: data.outputBusId.present
          ? data.outputBusId.value
          : this.outputBusId,
      fxJson: data.fxJson.present ? data.fxJson.value : this.fxJson,
      sendsJson: data.sendsJson.present ? data.sendsJson.value : this.sendsJson,
      orderIndex: data.orderIndex.present
          ? data.orderIndex.value
          : this.orderIndex,
    );
  }

  @override
  String toString() {
    return (StringBuffer('FxBusRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('practiceId: $practiceId, ')
          ..write('name: $name, ')
          ..write('colorValue: $colorValue, ')
          ..write('volume: $volume, ')
          ..write('pan: $pan, ')
          ..write('muted: $muted, ')
          ..write('soloed: $soloed, ')
          ..write('outputBusId: $outputBusId, ')
          ..write('fxJson: $fxJson, ')
          ..write('sendsJson: $sendsJson, ')
          ..write('orderIndex: $orderIndex')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    practiceId,
    name,
    colorValue,
    volume,
    pan,
    muted,
    soloed,
    outputBusId,
    fxJson,
    sendsJson,
    orderIndex,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is FxBusRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.practiceId == this.practiceId &&
          other.name == this.name &&
          other.colorValue == this.colorValue &&
          other.volume == this.volume &&
          other.pan == this.pan &&
          other.muted == this.muted &&
          other.soloed == this.soloed &&
          other.outputBusId == this.outputBusId &&
          other.fxJson == this.fxJson &&
          other.sendsJson == this.sendsJson &&
          other.orderIndex == this.orderIndex);
}

class FxBusesCompanion extends UpdateCompanion<FxBusRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> practiceId;
  final Value<String> name;
  final Value<int?> colorValue;
  final Value<double> volume;
  final Value<double> pan;
  final Value<bool> muted;
  final Value<bool> soloed;
  final Value<String?> outputBusId;
  final Value<String?> fxJson;
  final Value<String?> sendsJson;
  final Value<int> orderIndex;
  final Value<int> rowid;
  const FxBusesCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.practiceId = const Value.absent(),
    this.name = const Value.absent(),
    this.colorValue = const Value.absent(),
    this.volume = const Value.absent(),
    this.pan = const Value.absent(),
    this.muted = const Value.absent(),
    this.soloed = const Value.absent(),
    this.outputBusId = const Value.absent(),
    this.fxJson = const Value.absent(),
    this.sendsJson = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  FxBusesCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String practiceId,
    this.name = const Value.absent(),
    this.colorValue = const Value.absent(),
    this.volume = const Value.absent(),
    this.pan = const Value.absent(),
    this.muted = const Value.absent(),
    this.soloed = const Value.absent(),
    this.outputBusId = const Value.absent(),
    this.fxJson = const Value.absent(),
    this.sendsJson = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       practiceId = Value(practiceId);
  static Insertable<FxBusRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? practiceId,
    Expression<String>? name,
    Expression<int>? colorValue,
    Expression<double>? volume,
    Expression<double>? pan,
    Expression<bool>? muted,
    Expression<bool>? soloed,
    Expression<String>? outputBusId,
    Expression<String>? fxJson,
    Expression<String>? sendsJson,
    Expression<int>? orderIndex,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (practiceId != null) 'practice_id': practiceId,
      if (name != null) 'name': name,
      if (colorValue != null) 'color_value': colorValue,
      if (volume != null) 'volume': volume,
      if (pan != null) 'pan': pan,
      if (muted != null) 'muted': muted,
      if (soloed != null) 'soloed': soloed,
      if (outputBusId != null) 'output_bus_id': outputBusId,
      if (fxJson != null) 'fx_json': fxJson,
      if (sendsJson != null) 'sends_json': sendsJson,
      if (orderIndex != null) 'order_index': orderIndex,
      if (rowid != null) 'rowid': rowid,
    });
  }

  FxBusesCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? practiceId,
    Value<String>? name,
    Value<int?>? colorValue,
    Value<double>? volume,
    Value<double>? pan,
    Value<bool>? muted,
    Value<bool>? soloed,
    Value<String?>? outputBusId,
    Value<String?>? fxJson,
    Value<String?>? sendsJson,
    Value<int>? orderIndex,
    Value<int>? rowid,
  }) {
    return FxBusesCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      practiceId: practiceId ?? this.practiceId,
      name: name ?? this.name,
      colorValue: colorValue ?? this.colorValue,
      volume: volume ?? this.volume,
      pan: pan ?? this.pan,
      muted: muted ?? this.muted,
      soloed: soloed ?? this.soloed,
      outputBusId: outputBusId ?? this.outputBusId,
      fxJson: fxJson ?? this.fxJson,
      sendsJson: sendsJson ?? this.sendsJson,
      orderIndex: orderIndex ?? this.orderIndex,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (practiceId.present) {
      map['practice_id'] = Variable<String>(practiceId.value);
    }
    if (name.present) {
      map['name'] = Variable<String>(name.value);
    }
    if (colorValue.present) {
      map['color_value'] = Variable<int>(colorValue.value);
    }
    if (volume.present) {
      map['volume'] = Variable<double>(volume.value);
    }
    if (pan.present) {
      map['pan'] = Variable<double>(pan.value);
    }
    if (muted.present) {
      map['muted'] = Variable<bool>(muted.value);
    }
    if (soloed.present) {
      map['soloed'] = Variable<bool>(soloed.value);
    }
    if (outputBusId.present) {
      map['output_bus_id'] = Variable<String>(outputBusId.value);
    }
    if (fxJson.present) {
      map['fx_json'] = Variable<String>(fxJson.value);
    }
    if (sendsJson.present) {
      map['sends_json'] = Variable<String>(sendsJson.value);
    }
    if (orderIndex.present) {
      map['order_index'] = Variable<int>(orderIndex.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('FxBusesCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('practiceId: $practiceId, ')
          ..write('name: $name, ')
          ..write('colorValue: $colorValue, ')
          ..write('volume: $volume, ')
          ..write('pan: $pan, ')
          ..write('muted: $muted, ')
          ..write('soloed: $soloed, ')
          ..write('outputBusId: $outputBusId, ')
          ..write('fxJson: $fxJson, ')
          ..write('sendsJson: $sendsJson, ')
          ..write('orderIndex: $orderIndex, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $TracksTable extends Tracks with TableInfo<$TracksTable, TrackRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $TracksTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _practiceIdMeta = const VerificationMeta(
    'practiceId',
  );
  @override
  late final GeneratedColumn<String> practiceId = GeneratedColumn<String>(
    'practice_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES practices (id)',
    ),
  );
  static const VerificationMeta _kindMeta = const VerificationMeta('kind');
  @override
  late final GeneratedColumn<String> kind = GeneratedColumn<String>(
    'kind',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _interpretationIdMeta = const VerificationMeta(
    'interpretationId',
  );
  @override
  late final GeneratedColumn<String> interpretationId = GeneratedColumn<String>(
    'interpretation_id',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES interpretations (id)',
    ),
  );
  static const VerificationMeta _displayNameMeta = const VerificationMeta(
    'displayName',
  );
  @override
  late final GeneratedColumn<String> displayName = GeneratedColumn<String>(
    'display_name',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _colorValueMeta = const VerificationMeta(
    'colorValue',
  );
  @override
  late final GeneratedColumn<int> colorValue = GeneratedColumn<int>(
    'color_value',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _mutedMeta = const VerificationMeta('muted');
  @override
  late final GeneratedColumn<bool> muted = GeneratedColumn<bool>(
    'muted',
    aliasedName,
    false,
    type: DriftSqlType.bool,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'CHECK ("muted" IN (0, 1))',
    ),
    defaultValue: const Constant(false),
  );
  static const VerificationMeta _soloedMeta = const VerificationMeta('soloed');
  @override
  late final GeneratedColumn<bool> soloed = GeneratedColumn<bool>(
    'soloed',
    aliasedName,
    false,
    type: DriftSqlType.bool,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'CHECK ("soloed" IN (0, 1))',
    ),
    defaultValue: const Constant(false),
  );
  static const VerificationMeta _volumeMeta = const VerificationMeta('volume');
  @override
  late final GeneratedColumn<double> volume = GeneratedColumn<double>(
    'volume',
    aliasedName,
    false,
    type: DriftSqlType.double,
    requiredDuringInsert: false,
    defaultValue: const Constant(1.0),
  );
  static const VerificationMeta _panMeta = const VerificationMeta('pan');
  @override
  late final GeneratedColumn<double> pan = GeneratedColumn<double>(
    'pan',
    aliasedName,
    false,
    type: DriftSqlType.double,
    requiredDuringInsert: false,
    defaultValue: const Constant(0.0),
  );
  static const VerificationMeta _busIdMeta = const VerificationMeta('busId');
  @override
  late final GeneratedColumn<String> busId = GeneratedColumn<String>(
    'bus_id',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES fx_buses (id)',
    ),
  );
  static const VerificationMeta _fxJsonMeta = const VerificationMeta('fxJson');
  @override
  late final GeneratedColumn<String> fxJson = GeneratedColumn<String>(
    'fx_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _sendsJsonMeta = const VerificationMeta(
    'sendsJson',
  );
  @override
  late final GeneratedColumn<String> sendsJson = GeneratedColumn<String>(
    'sends_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _orderIndexMeta = const VerificationMeta(
    'orderIndex',
  );
  @override
  late final GeneratedColumn<int> orderIndex = GeneratedColumn<int>(
    'order_index',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    practiceId,
    kind,
    interpretationId,
    displayName,
    colorValue,
    muted,
    soloed,
    volume,
    pan,
    busId,
    fxJson,
    sendsJson,
    orderIndex,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'tracks';
  @override
  VerificationContext validateIntegrity(
    Insertable<TrackRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('practice_id')) {
      context.handle(
        _practiceIdMeta,
        practiceId.isAcceptableOrUnknown(data['practice_id']!, _practiceIdMeta),
      );
    } else if (isInserting) {
      context.missing(_practiceIdMeta);
    }
    if (data.containsKey('kind')) {
      context.handle(
        _kindMeta,
        kind.isAcceptableOrUnknown(data['kind']!, _kindMeta),
      );
    } else if (isInserting) {
      context.missing(_kindMeta);
    }
    if (data.containsKey('interpretation_id')) {
      context.handle(
        _interpretationIdMeta,
        interpretationId.isAcceptableOrUnknown(
          data['interpretation_id']!,
          _interpretationIdMeta,
        ),
      );
    }
    if (data.containsKey('display_name')) {
      context.handle(
        _displayNameMeta,
        displayName.isAcceptableOrUnknown(
          data['display_name']!,
          _displayNameMeta,
        ),
      );
    }
    if (data.containsKey('color_value')) {
      context.handle(
        _colorValueMeta,
        colorValue.isAcceptableOrUnknown(data['color_value']!, _colorValueMeta),
      );
    }
    if (data.containsKey('muted')) {
      context.handle(
        _mutedMeta,
        muted.isAcceptableOrUnknown(data['muted']!, _mutedMeta),
      );
    }
    if (data.containsKey('soloed')) {
      context.handle(
        _soloedMeta,
        soloed.isAcceptableOrUnknown(data['soloed']!, _soloedMeta),
      );
    }
    if (data.containsKey('volume')) {
      context.handle(
        _volumeMeta,
        volume.isAcceptableOrUnknown(data['volume']!, _volumeMeta),
      );
    }
    if (data.containsKey('pan')) {
      context.handle(
        _panMeta,
        pan.isAcceptableOrUnknown(data['pan']!, _panMeta),
      );
    }
    if (data.containsKey('bus_id')) {
      context.handle(
        _busIdMeta,
        busId.isAcceptableOrUnknown(data['bus_id']!, _busIdMeta),
      );
    }
    if (data.containsKey('fx_json')) {
      context.handle(
        _fxJsonMeta,
        fxJson.isAcceptableOrUnknown(data['fx_json']!, _fxJsonMeta),
      );
    }
    if (data.containsKey('sends_json')) {
      context.handle(
        _sendsJsonMeta,
        sendsJson.isAcceptableOrUnknown(data['sends_json']!, _sendsJsonMeta),
      );
    }
    if (data.containsKey('order_index')) {
      context.handle(
        _orderIndexMeta,
        orderIndex.isAcceptableOrUnknown(data['order_index']!, _orderIndexMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  TrackRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return TrackRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      practiceId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}practice_id'],
      )!,
      kind: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}kind'],
      )!,
      interpretationId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}interpretation_id'],
      ),
      displayName: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}display_name'],
      )!,
      colorValue: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}color_value'],
      ),
      muted: attachedDatabase.typeMapping.read(
        DriftSqlType.bool,
        data['${effectivePrefix}muted'],
      )!,
      soloed: attachedDatabase.typeMapping.read(
        DriftSqlType.bool,
        data['${effectivePrefix}soloed'],
      )!,
      volume: attachedDatabase.typeMapping.read(
        DriftSqlType.double,
        data['${effectivePrefix}volume'],
      )!,
      pan: attachedDatabase.typeMapping.read(
        DriftSqlType.double,
        data['${effectivePrefix}pan'],
      )!,
      busId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}bus_id'],
      ),
      fxJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}fx_json'],
      ),
      sendsJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}sends_json'],
      ),
      orderIndex: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}order_index'],
      )!,
    );
  }

  @override
  $TracksTable createAlias(String alias) {
    return $TracksTable(attachedDatabase, alias);
  }
}

class TrackRow extends DataClass implements Insertable<TrackRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String practiceId;
  final String kind;
  final String? interpretationId;
  final String displayName;
  final int? colorValue;
  final bool muted;
  final bool soloed;
  final double volume;
  final double pan;
  final String? busId;
  final String? fxJson;
  final String? sendsJson;
  final int orderIndex;
  const TrackRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.practiceId,
    required this.kind,
    this.interpretationId,
    required this.displayName,
    this.colorValue,
    required this.muted,
    required this.soloed,
    required this.volume,
    required this.pan,
    this.busId,
    this.fxJson,
    this.sendsJson,
    required this.orderIndex,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['practice_id'] = Variable<String>(practiceId);
    map['kind'] = Variable<String>(kind);
    if (!nullToAbsent || interpretationId != null) {
      map['interpretation_id'] = Variable<String>(interpretationId);
    }
    map['display_name'] = Variable<String>(displayName);
    if (!nullToAbsent || colorValue != null) {
      map['color_value'] = Variable<int>(colorValue);
    }
    map['muted'] = Variable<bool>(muted);
    map['soloed'] = Variable<bool>(soloed);
    map['volume'] = Variable<double>(volume);
    map['pan'] = Variable<double>(pan);
    if (!nullToAbsent || busId != null) {
      map['bus_id'] = Variable<String>(busId);
    }
    if (!nullToAbsent || fxJson != null) {
      map['fx_json'] = Variable<String>(fxJson);
    }
    if (!nullToAbsent || sendsJson != null) {
      map['sends_json'] = Variable<String>(sendsJson);
    }
    map['order_index'] = Variable<int>(orderIndex);
    return map;
  }

  TracksCompanion toCompanion(bool nullToAbsent) {
    return TracksCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      practiceId: Value(practiceId),
      kind: Value(kind),
      interpretationId: interpretationId == null && nullToAbsent
          ? const Value.absent()
          : Value(interpretationId),
      displayName: Value(displayName),
      colorValue: colorValue == null && nullToAbsent
          ? const Value.absent()
          : Value(colorValue),
      muted: Value(muted),
      soloed: Value(soloed),
      volume: Value(volume),
      pan: Value(pan),
      busId: busId == null && nullToAbsent
          ? const Value.absent()
          : Value(busId),
      fxJson: fxJson == null && nullToAbsent
          ? const Value.absent()
          : Value(fxJson),
      sendsJson: sendsJson == null && nullToAbsent
          ? const Value.absent()
          : Value(sendsJson),
      orderIndex: Value(orderIndex),
    );
  }

  factory TrackRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return TrackRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      practiceId: serializer.fromJson<String>(json['practiceId']),
      kind: serializer.fromJson<String>(json['kind']),
      interpretationId: serializer.fromJson<String?>(json['interpretationId']),
      displayName: serializer.fromJson<String>(json['displayName']),
      colorValue: serializer.fromJson<int?>(json['colorValue']),
      muted: serializer.fromJson<bool>(json['muted']),
      soloed: serializer.fromJson<bool>(json['soloed']),
      volume: serializer.fromJson<double>(json['volume']),
      pan: serializer.fromJson<double>(json['pan']),
      busId: serializer.fromJson<String?>(json['busId']),
      fxJson: serializer.fromJson<String?>(json['fxJson']),
      sendsJson: serializer.fromJson<String?>(json['sendsJson']),
      orderIndex: serializer.fromJson<int>(json['orderIndex']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'practiceId': serializer.toJson<String>(practiceId),
      'kind': serializer.toJson<String>(kind),
      'interpretationId': serializer.toJson<String?>(interpretationId),
      'displayName': serializer.toJson<String>(displayName),
      'colorValue': serializer.toJson<int?>(colorValue),
      'muted': serializer.toJson<bool>(muted),
      'soloed': serializer.toJson<bool>(soloed),
      'volume': serializer.toJson<double>(volume),
      'pan': serializer.toJson<double>(pan),
      'busId': serializer.toJson<String?>(busId),
      'fxJson': serializer.toJson<String?>(fxJson),
      'sendsJson': serializer.toJson<String?>(sendsJson),
      'orderIndex': serializer.toJson<int>(orderIndex),
    };
  }

  TrackRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? practiceId,
    String? kind,
    Value<String?> interpretationId = const Value.absent(),
    String? displayName,
    Value<int?> colorValue = const Value.absent(),
    bool? muted,
    bool? soloed,
    double? volume,
    double? pan,
    Value<String?> busId = const Value.absent(),
    Value<String?> fxJson = const Value.absent(),
    Value<String?> sendsJson = const Value.absent(),
    int? orderIndex,
  }) => TrackRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    practiceId: practiceId ?? this.practiceId,
    kind: kind ?? this.kind,
    interpretationId: interpretationId.present
        ? interpretationId.value
        : this.interpretationId,
    displayName: displayName ?? this.displayName,
    colorValue: colorValue.present ? colorValue.value : this.colorValue,
    muted: muted ?? this.muted,
    soloed: soloed ?? this.soloed,
    volume: volume ?? this.volume,
    pan: pan ?? this.pan,
    busId: busId.present ? busId.value : this.busId,
    fxJson: fxJson.present ? fxJson.value : this.fxJson,
    sendsJson: sendsJson.present ? sendsJson.value : this.sendsJson,
    orderIndex: orderIndex ?? this.orderIndex,
  );
  TrackRow copyWithCompanion(TracksCompanion data) {
    return TrackRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      practiceId: data.practiceId.present
          ? data.practiceId.value
          : this.practiceId,
      kind: data.kind.present ? data.kind.value : this.kind,
      interpretationId: data.interpretationId.present
          ? data.interpretationId.value
          : this.interpretationId,
      displayName: data.displayName.present
          ? data.displayName.value
          : this.displayName,
      colorValue: data.colorValue.present
          ? data.colorValue.value
          : this.colorValue,
      muted: data.muted.present ? data.muted.value : this.muted,
      soloed: data.soloed.present ? data.soloed.value : this.soloed,
      volume: data.volume.present ? data.volume.value : this.volume,
      pan: data.pan.present ? data.pan.value : this.pan,
      busId: data.busId.present ? data.busId.value : this.busId,
      fxJson: data.fxJson.present ? data.fxJson.value : this.fxJson,
      sendsJson: data.sendsJson.present ? data.sendsJson.value : this.sendsJson,
      orderIndex: data.orderIndex.present
          ? data.orderIndex.value
          : this.orderIndex,
    );
  }

  @override
  String toString() {
    return (StringBuffer('TrackRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('practiceId: $practiceId, ')
          ..write('kind: $kind, ')
          ..write('interpretationId: $interpretationId, ')
          ..write('displayName: $displayName, ')
          ..write('colorValue: $colorValue, ')
          ..write('muted: $muted, ')
          ..write('soloed: $soloed, ')
          ..write('volume: $volume, ')
          ..write('pan: $pan, ')
          ..write('busId: $busId, ')
          ..write('fxJson: $fxJson, ')
          ..write('sendsJson: $sendsJson, ')
          ..write('orderIndex: $orderIndex')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hashAll([
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    practiceId,
    kind,
    interpretationId,
    displayName,
    colorValue,
    muted,
    soloed,
    volume,
    pan,
    busId,
    fxJson,
    sendsJson,
    orderIndex,
  ]);
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is TrackRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.practiceId == this.practiceId &&
          other.kind == this.kind &&
          other.interpretationId == this.interpretationId &&
          other.displayName == this.displayName &&
          other.colorValue == this.colorValue &&
          other.muted == this.muted &&
          other.soloed == this.soloed &&
          other.volume == this.volume &&
          other.pan == this.pan &&
          other.busId == this.busId &&
          other.fxJson == this.fxJson &&
          other.sendsJson == this.sendsJson &&
          other.orderIndex == this.orderIndex);
}

class TracksCompanion extends UpdateCompanion<TrackRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> practiceId;
  final Value<String> kind;
  final Value<String?> interpretationId;
  final Value<String> displayName;
  final Value<int?> colorValue;
  final Value<bool> muted;
  final Value<bool> soloed;
  final Value<double> volume;
  final Value<double> pan;
  final Value<String?> busId;
  final Value<String?> fxJson;
  final Value<String?> sendsJson;
  final Value<int> orderIndex;
  final Value<int> rowid;
  const TracksCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.practiceId = const Value.absent(),
    this.kind = const Value.absent(),
    this.interpretationId = const Value.absent(),
    this.displayName = const Value.absent(),
    this.colorValue = const Value.absent(),
    this.muted = const Value.absent(),
    this.soloed = const Value.absent(),
    this.volume = const Value.absent(),
    this.pan = const Value.absent(),
    this.busId = const Value.absent(),
    this.fxJson = const Value.absent(),
    this.sendsJson = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  TracksCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String practiceId,
    required String kind,
    this.interpretationId = const Value.absent(),
    this.displayName = const Value.absent(),
    this.colorValue = const Value.absent(),
    this.muted = const Value.absent(),
    this.soloed = const Value.absent(),
    this.volume = const Value.absent(),
    this.pan = const Value.absent(),
    this.busId = const Value.absent(),
    this.fxJson = const Value.absent(),
    this.sendsJson = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       practiceId = Value(practiceId),
       kind = Value(kind);
  static Insertable<TrackRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? practiceId,
    Expression<String>? kind,
    Expression<String>? interpretationId,
    Expression<String>? displayName,
    Expression<int>? colorValue,
    Expression<bool>? muted,
    Expression<bool>? soloed,
    Expression<double>? volume,
    Expression<double>? pan,
    Expression<String>? busId,
    Expression<String>? fxJson,
    Expression<String>? sendsJson,
    Expression<int>? orderIndex,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (practiceId != null) 'practice_id': practiceId,
      if (kind != null) 'kind': kind,
      if (interpretationId != null) 'interpretation_id': interpretationId,
      if (displayName != null) 'display_name': displayName,
      if (colorValue != null) 'color_value': colorValue,
      if (muted != null) 'muted': muted,
      if (soloed != null) 'soloed': soloed,
      if (volume != null) 'volume': volume,
      if (pan != null) 'pan': pan,
      if (busId != null) 'bus_id': busId,
      if (fxJson != null) 'fx_json': fxJson,
      if (sendsJson != null) 'sends_json': sendsJson,
      if (orderIndex != null) 'order_index': orderIndex,
      if (rowid != null) 'rowid': rowid,
    });
  }

  TracksCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? practiceId,
    Value<String>? kind,
    Value<String?>? interpretationId,
    Value<String>? displayName,
    Value<int?>? colorValue,
    Value<bool>? muted,
    Value<bool>? soloed,
    Value<double>? volume,
    Value<double>? pan,
    Value<String?>? busId,
    Value<String?>? fxJson,
    Value<String?>? sendsJson,
    Value<int>? orderIndex,
    Value<int>? rowid,
  }) {
    return TracksCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      practiceId: practiceId ?? this.practiceId,
      kind: kind ?? this.kind,
      interpretationId: interpretationId ?? this.interpretationId,
      displayName: displayName ?? this.displayName,
      colorValue: colorValue ?? this.colorValue,
      muted: muted ?? this.muted,
      soloed: soloed ?? this.soloed,
      volume: volume ?? this.volume,
      pan: pan ?? this.pan,
      busId: busId ?? this.busId,
      fxJson: fxJson ?? this.fxJson,
      sendsJson: sendsJson ?? this.sendsJson,
      orderIndex: orderIndex ?? this.orderIndex,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (practiceId.present) {
      map['practice_id'] = Variable<String>(practiceId.value);
    }
    if (kind.present) {
      map['kind'] = Variable<String>(kind.value);
    }
    if (interpretationId.present) {
      map['interpretation_id'] = Variable<String>(interpretationId.value);
    }
    if (displayName.present) {
      map['display_name'] = Variable<String>(displayName.value);
    }
    if (colorValue.present) {
      map['color_value'] = Variable<int>(colorValue.value);
    }
    if (muted.present) {
      map['muted'] = Variable<bool>(muted.value);
    }
    if (soloed.present) {
      map['soloed'] = Variable<bool>(soloed.value);
    }
    if (volume.present) {
      map['volume'] = Variable<double>(volume.value);
    }
    if (pan.present) {
      map['pan'] = Variable<double>(pan.value);
    }
    if (busId.present) {
      map['bus_id'] = Variable<String>(busId.value);
    }
    if (fxJson.present) {
      map['fx_json'] = Variable<String>(fxJson.value);
    }
    if (sendsJson.present) {
      map['sends_json'] = Variable<String>(sendsJson.value);
    }
    if (orderIndex.present) {
      map['order_index'] = Variable<int>(orderIndex.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('TracksCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('practiceId: $practiceId, ')
          ..write('kind: $kind, ')
          ..write('interpretationId: $interpretationId, ')
          ..write('displayName: $displayName, ')
          ..write('colorValue: $colorValue, ')
          ..write('muted: $muted, ')
          ..write('soloed: $soloed, ')
          ..write('volume: $volume, ')
          ..write('pan: $pan, ')
          ..write('busId: $busId, ')
          ..write('fxJson: $fxJson, ')
          ..write('sendsJson: $sendsJson, ')
          ..write('orderIndex: $orderIndex, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $ClipsTable extends Clips with TableInfo<$ClipsTable, ClipRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $ClipsTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _trackIdMeta = const VerificationMeta(
    'trackId',
  );
  @override
  late final GeneratedColumn<String> trackId = GeneratedColumn<String>(
    'track_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES tracks (id)',
    ),
  );
  static const VerificationMeta _assetIdMeta = const VerificationMeta(
    'assetId',
  );
  @override
  late final GeneratedColumn<String> assetId = GeneratedColumn<String>(
    'asset_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES assets (id)',
    ),
  );
  static const VerificationMeta _startMsMeta = const VerificationMeta(
    'startMs',
  );
  @override
  late final GeneratedColumn<int> startMs = GeneratedColumn<int>(
    'start_ms',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  static const VerificationMeta _durationMsMeta = const VerificationMeta(
    'durationMs',
  );
  @override
  late final GeneratedColumn<int> durationMs = GeneratedColumn<int>(
    'duration_ms',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  static const VerificationMeta _orderIndexMeta = const VerificationMeta(
    'orderIndex',
  );
  @override
  late final GeneratedColumn<int> orderIndex = GeneratedColumn<int>(
    'order_index',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    trackId,
    assetId,
    startMs,
    durationMs,
    orderIndex,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'clips';
  @override
  VerificationContext validateIntegrity(
    Insertable<ClipRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('track_id')) {
      context.handle(
        _trackIdMeta,
        trackId.isAcceptableOrUnknown(data['track_id']!, _trackIdMeta),
      );
    } else if (isInserting) {
      context.missing(_trackIdMeta);
    }
    if (data.containsKey('asset_id')) {
      context.handle(
        _assetIdMeta,
        assetId.isAcceptableOrUnknown(data['asset_id']!, _assetIdMeta),
      );
    } else if (isInserting) {
      context.missing(_assetIdMeta);
    }
    if (data.containsKey('start_ms')) {
      context.handle(
        _startMsMeta,
        startMs.isAcceptableOrUnknown(data['start_ms']!, _startMsMeta),
      );
    }
    if (data.containsKey('duration_ms')) {
      context.handle(
        _durationMsMeta,
        durationMs.isAcceptableOrUnknown(data['duration_ms']!, _durationMsMeta),
      );
    }
    if (data.containsKey('order_index')) {
      context.handle(
        _orderIndexMeta,
        orderIndex.isAcceptableOrUnknown(data['order_index']!, _orderIndexMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  ClipRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return ClipRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      trackId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}track_id'],
      )!,
      assetId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}asset_id'],
      )!,
      startMs: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}start_ms'],
      )!,
      durationMs: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}duration_ms'],
      )!,
      orderIndex: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}order_index'],
      )!,
    );
  }

  @override
  $ClipsTable createAlias(String alias) {
    return $ClipsTable(attachedDatabase, alias);
  }
}

class ClipRow extends DataClass implements Insertable<ClipRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String trackId;
  final String assetId;
  final int startMs;
  final int durationMs;
  final int orderIndex;
  const ClipRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.trackId,
    required this.assetId,
    required this.startMs,
    required this.durationMs,
    required this.orderIndex,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['track_id'] = Variable<String>(trackId);
    map['asset_id'] = Variable<String>(assetId);
    map['start_ms'] = Variable<int>(startMs);
    map['duration_ms'] = Variable<int>(durationMs);
    map['order_index'] = Variable<int>(orderIndex);
    return map;
  }

  ClipsCompanion toCompanion(bool nullToAbsent) {
    return ClipsCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      trackId: Value(trackId),
      assetId: Value(assetId),
      startMs: Value(startMs),
      durationMs: Value(durationMs),
      orderIndex: Value(orderIndex),
    );
  }

  factory ClipRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return ClipRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      trackId: serializer.fromJson<String>(json['trackId']),
      assetId: serializer.fromJson<String>(json['assetId']),
      startMs: serializer.fromJson<int>(json['startMs']),
      durationMs: serializer.fromJson<int>(json['durationMs']),
      orderIndex: serializer.fromJson<int>(json['orderIndex']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'trackId': serializer.toJson<String>(trackId),
      'assetId': serializer.toJson<String>(assetId),
      'startMs': serializer.toJson<int>(startMs),
      'durationMs': serializer.toJson<int>(durationMs),
      'orderIndex': serializer.toJson<int>(orderIndex),
    };
  }

  ClipRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? trackId,
    String? assetId,
    int? startMs,
    int? durationMs,
    int? orderIndex,
  }) => ClipRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    trackId: trackId ?? this.trackId,
    assetId: assetId ?? this.assetId,
    startMs: startMs ?? this.startMs,
    durationMs: durationMs ?? this.durationMs,
    orderIndex: orderIndex ?? this.orderIndex,
  );
  ClipRow copyWithCompanion(ClipsCompanion data) {
    return ClipRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      trackId: data.trackId.present ? data.trackId.value : this.trackId,
      assetId: data.assetId.present ? data.assetId.value : this.assetId,
      startMs: data.startMs.present ? data.startMs.value : this.startMs,
      durationMs: data.durationMs.present
          ? data.durationMs.value
          : this.durationMs,
      orderIndex: data.orderIndex.present
          ? data.orderIndex.value
          : this.orderIndex,
    );
  }

  @override
  String toString() {
    return (StringBuffer('ClipRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('trackId: $trackId, ')
          ..write('assetId: $assetId, ')
          ..write('startMs: $startMs, ')
          ..write('durationMs: $durationMs, ')
          ..write('orderIndex: $orderIndex')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    trackId,
    assetId,
    startMs,
    durationMs,
    orderIndex,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is ClipRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.trackId == this.trackId &&
          other.assetId == this.assetId &&
          other.startMs == this.startMs &&
          other.durationMs == this.durationMs &&
          other.orderIndex == this.orderIndex);
}

class ClipsCompanion extends UpdateCompanion<ClipRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> trackId;
  final Value<String> assetId;
  final Value<int> startMs;
  final Value<int> durationMs;
  final Value<int> orderIndex;
  final Value<int> rowid;
  const ClipsCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.trackId = const Value.absent(),
    this.assetId = const Value.absent(),
    this.startMs = const Value.absent(),
    this.durationMs = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  ClipsCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String trackId,
    required String assetId,
    this.startMs = const Value.absent(),
    this.durationMs = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       trackId = Value(trackId),
       assetId = Value(assetId);
  static Insertable<ClipRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? trackId,
    Expression<String>? assetId,
    Expression<int>? startMs,
    Expression<int>? durationMs,
    Expression<int>? orderIndex,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (trackId != null) 'track_id': trackId,
      if (assetId != null) 'asset_id': assetId,
      if (startMs != null) 'start_ms': startMs,
      if (durationMs != null) 'duration_ms': durationMs,
      if (orderIndex != null) 'order_index': orderIndex,
      if (rowid != null) 'rowid': rowid,
    });
  }

  ClipsCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? trackId,
    Value<String>? assetId,
    Value<int>? startMs,
    Value<int>? durationMs,
    Value<int>? orderIndex,
    Value<int>? rowid,
  }) {
    return ClipsCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      trackId: trackId ?? this.trackId,
      assetId: assetId ?? this.assetId,
      startMs: startMs ?? this.startMs,
      durationMs: durationMs ?? this.durationMs,
      orderIndex: orderIndex ?? this.orderIndex,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (trackId.present) {
      map['track_id'] = Variable<String>(trackId.value);
    }
    if (assetId.present) {
      map['asset_id'] = Variable<String>(assetId.value);
    }
    if (startMs.present) {
      map['start_ms'] = Variable<int>(startMs.value);
    }
    if (durationMs.present) {
      map['duration_ms'] = Variable<int>(durationMs.value);
    }
    if (orderIndex.present) {
      map['order_index'] = Variable<int>(orderIndex.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('ClipsCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('trackId: $trackId, ')
          ..write('assetId: $assetId, ')
          ..write('startMs: $startMs, ')
          ..write('durationMs: $durationMs, ')
          ..write('orderIndex: $orderIndex, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $PracticeNotesTable extends PracticeNotes
    with TableInfo<$PracticeNotesTable, PracticeNoteRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $PracticeNotesTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _trackIdMeta = const VerificationMeta(
    'trackId',
  );
  @override
  late final GeneratedColumn<String> trackId = GeneratedColumn<String>(
    'track_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES tracks (id)',
    ),
  );
  static const VerificationMeta _positionMsMeta = const VerificationMeta(
    'positionMs',
  );
  @override
  late final GeneratedColumn<int> positionMs = GeneratedColumn<int>(
    'position_ms',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  static const VerificationMeta _endMsMeta = const VerificationMeta('endMs');
  @override
  late final GeneratedColumn<int> endMs = GeneratedColumn<int>(
    'end_ms',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _contentMeta = const VerificationMeta(
    'content',
  );
  @override
  late final GeneratedColumn<String> content = GeneratedColumn<String>(
    'content',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    trackId,
    positionMs,
    endMs,
    content,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'practice_notes';
  @override
  VerificationContext validateIntegrity(
    Insertable<PracticeNoteRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('track_id')) {
      context.handle(
        _trackIdMeta,
        trackId.isAcceptableOrUnknown(data['track_id']!, _trackIdMeta),
      );
    } else if (isInserting) {
      context.missing(_trackIdMeta);
    }
    if (data.containsKey('position_ms')) {
      context.handle(
        _positionMsMeta,
        positionMs.isAcceptableOrUnknown(data['position_ms']!, _positionMsMeta),
      );
    }
    if (data.containsKey('end_ms')) {
      context.handle(
        _endMsMeta,
        endMs.isAcceptableOrUnknown(data['end_ms']!, _endMsMeta),
      );
    }
    if (data.containsKey('content')) {
      context.handle(
        _contentMeta,
        content.isAcceptableOrUnknown(data['content']!, _contentMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  PracticeNoteRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return PracticeNoteRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      trackId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}track_id'],
      )!,
      positionMs: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}position_ms'],
      )!,
      endMs: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}end_ms'],
      ),
      content: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}content'],
      )!,
    );
  }

  @override
  $PracticeNotesTable createAlias(String alias) {
    return $PracticeNotesTable(attachedDatabase, alias);
  }
}

class PracticeNoteRow extends DataClass implements Insertable<PracticeNoteRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String trackId;
  final int positionMs;
  final int? endMs;
  final String content;
  const PracticeNoteRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.trackId,
    required this.positionMs,
    this.endMs,
    required this.content,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['track_id'] = Variable<String>(trackId);
    map['position_ms'] = Variable<int>(positionMs);
    if (!nullToAbsent || endMs != null) {
      map['end_ms'] = Variable<int>(endMs);
    }
    map['content'] = Variable<String>(content);
    return map;
  }

  PracticeNotesCompanion toCompanion(bool nullToAbsent) {
    return PracticeNotesCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      trackId: Value(trackId),
      positionMs: Value(positionMs),
      endMs: endMs == null && nullToAbsent
          ? const Value.absent()
          : Value(endMs),
      content: Value(content),
    );
  }

  factory PracticeNoteRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return PracticeNoteRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      trackId: serializer.fromJson<String>(json['trackId']),
      positionMs: serializer.fromJson<int>(json['positionMs']),
      endMs: serializer.fromJson<int?>(json['endMs']),
      content: serializer.fromJson<String>(json['content']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'trackId': serializer.toJson<String>(trackId),
      'positionMs': serializer.toJson<int>(positionMs),
      'endMs': serializer.toJson<int?>(endMs),
      'content': serializer.toJson<String>(content),
    };
  }

  PracticeNoteRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? trackId,
    int? positionMs,
    Value<int?> endMs = const Value.absent(),
    String? content,
  }) => PracticeNoteRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    trackId: trackId ?? this.trackId,
    positionMs: positionMs ?? this.positionMs,
    endMs: endMs.present ? endMs.value : this.endMs,
    content: content ?? this.content,
  );
  PracticeNoteRow copyWithCompanion(PracticeNotesCompanion data) {
    return PracticeNoteRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      trackId: data.trackId.present ? data.trackId.value : this.trackId,
      positionMs: data.positionMs.present
          ? data.positionMs.value
          : this.positionMs,
      endMs: data.endMs.present ? data.endMs.value : this.endMs,
      content: data.content.present ? data.content.value : this.content,
    );
  }

  @override
  String toString() {
    return (StringBuffer('PracticeNoteRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('trackId: $trackId, ')
          ..write('positionMs: $positionMs, ')
          ..write('endMs: $endMs, ')
          ..write('content: $content')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    trackId,
    positionMs,
    endMs,
    content,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is PracticeNoteRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.trackId == this.trackId &&
          other.positionMs == this.positionMs &&
          other.endMs == this.endMs &&
          other.content == this.content);
}

class PracticeNotesCompanion extends UpdateCompanion<PracticeNoteRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> trackId;
  final Value<int> positionMs;
  final Value<int?> endMs;
  final Value<String> content;
  final Value<int> rowid;
  const PracticeNotesCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.trackId = const Value.absent(),
    this.positionMs = const Value.absent(),
    this.endMs = const Value.absent(),
    this.content = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  PracticeNotesCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String trackId,
    this.positionMs = const Value.absent(),
    this.endMs = const Value.absent(),
    this.content = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       trackId = Value(trackId);
  static Insertable<PracticeNoteRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? trackId,
    Expression<int>? positionMs,
    Expression<int>? endMs,
    Expression<String>? content,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (trackId != null) 'track_id': trackId,
      if (positionMs != null) 'position_ms': positionMs,
      if (endMs != null) 'end_ms': endMs,
      if (content != null) 'content': content,
      if (rowid != null) 'rowid': rowid,
    });
  }

  PracticeNotesCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? trackId,
    Value<int>? positionMs,
    Value<int?>? endMs,
    Value<String>? content,
    Value<int>? rowid,
  }) {
    return PracticeNotesCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      trackId: trackId ?? this.trackId,
      positionMs: positionMs ?? this.positionMs,
      endMs: endMs ?? this.endMs,
      content: content ?? this.content,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (trackId.present) {
      map['track_id'] = Variable<String>(trackId.value);
    }
    if (positionMs.present) {
      map['position_ms'] = Variable<int>(positionMs.value);
    }
    if (endMs.present) {
      map['end_ms'] = Variable<int>(endMs.value);
    }
    if (content.present) {
      map['content'] = Variable<String>(content.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('PracticeNotesCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('trackId: $trackId, ')
          ..write('positionMs: $positionMs, ')
          ..write('endMs: $endMs, ')
          ..write('content: $content, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $TechniqueMarkersTable extends TechniqueMarkers
    with TableInfo<$TechniqueMarkersTable, TechniqueMarkerRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $TechniqueMarkersTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _trackIdMeta = const VerificationMeta(
    'trackId',
  );
  @override
  late final GeneratedColumn<String> trackId = GeneratedColumn<String>(
    'track_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES tracks (id)',
    ),
  );
  static const VerificationMeta _positionMsMeta = const VerificationMeta(
    'positionMs',
  );
  @override
  late final GeneratedColumn<int> positionMs = GeneratedColumn<int>(
    'position_ms',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  static const VerificationMeta _endMsMeta = const VerificationMeta('endMs');
  @override
  late final GeneratedColumn<int> endMs = GeneratedColumn<int>(
    'end_ms',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _techniqueIdMeta = const VerificationMeta(
    'techniqueId',
  );
  @override
  late final GeneratedColumn<String> techniqueId = GeneratedColumn<String>(
    'technique_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _techniqueNameSnapshotMeta =
      const VerificationMeta('techniqueNameSnapshot');
  @override
  late final GeneratedColumn<String> techniqueNameSnapshot =
      GeneratedColumn<String>(
        'technique_name_snapshot',
        aliasedName,
        false,
        type: DriftSqlType.string,
        requiredDuringInsert: true,
      );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    trackId,
    positionMs,
    endMs,
    techniqueId,
    techniqueNameSnapshot,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'technique_markers';
  @override
  VerificationContext validateIntegrity(
    Insertable<TechniqueMarkerRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('track_id')) {
      context.handle(
        _trackIdMeta,
        trackId.isAcceptableOrUnknown(data['track_id']!, _trackIdMeta),
      );
    } else if (isInserting) {
      context.missing(_trackIdMeta);
    }
    if (data.containsKey('position_ms')) {
      context.handle(
        _positionMsMeta,
        positionMs.isAcceptableOrUnknown(data['position_ms']!, _positionMsMeta),
      );
    }
    if (data.containsKey('end_ms')) {
      context.handle(
        _endMsMeta,
        endMs.isAcceptableOrUnknown(data['end_ms']!, _endMsMeta),
      );
    }
    if (data.containsKey('technique_id')) {
      context.handle(
        _techniqueIdMeta,
        techniqueId.isAcceptableOrUnknown(
          data['technique_id']!,
          _techniqueIdMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_techniqueIdMeta);
    }
    if (data.containsKey('technique_name_snapshot')) {
      context.handle(
        _techniqueNameSnapshotMeta,
        techniqueNameSnapshot.isAcceptableOrUnknown(
          data['technique_name_snapshot']!,
          _techniqueNameSnapshotMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_techniqueNameSnapshotMeta);
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  TechniqueMarkerRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return TechniqueMarkerRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      trackId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}track_id'],
      )!,
      positionMs: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}position_ms'],
      )!,
      endMs: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}end_ms'],
      ),
      techniqueId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}technique_id'],
      )!,
      techniqueNameSnapshot: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}technique_name_snapshot'],
      )!,
    );
  }

  @override
  $TechniqueMarkersTable createAlias(String alias) {
    return $TechniqueMarkersTable(attachedDatabase, alias);
  }
}

class TechniqueMarkerRow extends DataClass
    implements Insertable<TechniqueMarkerRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String trackId;
  final int positionMs;
  final int? endMs;
  final String techniqueId;
  final String techniqueNameSnapshot;
  const TechniqueMarkerRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.trackId,
    required this.positionMs,
    this.endMs,
    required this.techniqueId,
    required this.techniqueNameSnapshot,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['track_id'] = Variable<String>(trackId);
    map['position_ms'] = Variable<int>(positionMs);
    if (!nullToAbsent || endMs != null) {
      map['end_ms'] = Variable<int>(endMs);
    }
    map['technique_id'] = Variable<String>(techniqueId);
    map['technique_name_snapshot'] = Variable<String>(techniqueNameSnapshot);
    return map;
  }

  TechniqueMarkersCompanion toCompanion(bool nullToAbsent) {
    return TechniqueMarkersCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      trackId: Value(trackId),
      positionMs: Value(positionMs),
      endMs: endMs == null && nullToAbsent
          ? const Value.absent()
          : Value(endMs),
      techniqueId: Value(techniqueId),
      techniqueNameSnapshot: Value(techniqueNameSnapshot),
    );
  }

  factory TechniqueMarkerRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return TechniqueMarkerRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      trackId: serializer.fromJson<String>(json['trackId']),
      positionMs: serializer.fromJson<int>(json['positionMs']),
      endMs: serializer.fromJson<int?>(json['endMs']),
      techniqueId: serializer.fromJson<String>(json['techniqueId']),
      techniqueNameSnapshot: serializer.fromJson<String>(
        json['techniqueNameSnapshot'],
      ),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'trackId': serializer.toJson<String>(trackId),
      'positionMs': serializer.toJson<int>(positionMs),
      'endMs': serializer.toJson<int?>(endMs),
      'techniqueId': serializer.toJson<String>(techniqueId),
      'techniqueNameSnapshot': serializer.toJson<String>(techniqueNameSnapshot),
    };
  }

  TechniqueMarkerRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? trackId,
    int? positionMs,
    Value<int?> endMs = const Value.absent(),
    String? techniqueId,
    String? techniqueNameSnapshot,
  }) => TechniqueMarkerRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    trackId: trackId ?? this.trackId,
    positionMs: positionMs ?? this.positionMs,
    endMs: endMs.present ? endMs.value : this.endMs,
    techniqueId: techniqueId ?? this.techniqueId,
    techniqueNameSnapshot: techniqueNameSnapshot ?? this.techniqueNameSnapshot,
  );
  TechniqueMarkerRow copyWithCompanion(TechniqueMarkersCompanion data) {
    return TechniqueMarkerRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      trackId: data.trackId.present ? data.trackId.value : this.trackId,
      positionMs: data.positionMs.present
          ? data.positionMs.value
          : this.positionMs,
      endMs: data.endMs.present ? data.endMs.value : this.endMs,
      techniqueId: data.techniqueId.present
          ? data.techniqueId.value
          : this.techniqueId,
      techniqueNameSnapshot: data.techniqueNameSnapshot.present
          ? data.techniqueNameSnapshot.value
          : this.techniqueNameSnapshot,
    );
  }

  @override
  String toString() {
    return (StringBuffer('TechniqueMarkerRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('trackId: $trackId, ')
          ..write('positionMs: $positionMs, ')
          ..write('endMs: $endMs, ')
          ..write('techniqueId: $techniqueId, ')
          ..write('techniqueNameSnapshot: $techniqueNameSnapshot')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    trackId,
    positionMs,
    endMs,
    techniqueId,
    techniqueNameSnapshot,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is TechniqueMarkerRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.trackId == this.trackId &&
          other.positionMs == this.positionMs &&
          other.endMs == this.endMs &&
          other.techniqueId == this.techniqueId &&
          other.techniqueNameSnapshot == this.techniqueNameSnapshot);
}

class TechniqueMarkersCompanion extends UpdateCompanion<TechniqueMarkerRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> trackId;
  final Value<int> positionMs;
  final Value<int?> endMs;
  final Value<String> techniqueId;
  final Value<String> techniqueNameSnapshot;
  final Value<int> rowid;
  const TechniqueMarkersCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.trackId = const Value.absent(),
    this.positionMs = const Value.absent(),
    this.endMs = const Value.absent(),
    this.techniqueId = const Value.absent(),
    this.techniqueNameSnapshot = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  TechniqueMarkersCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String trackId,
    this.positionMs = const Value.absent(),
    this.endMs = const Value.absent(),
    required String techniqueId,
    required String techniqueNameSnapshot,
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       trackId = Value(trackId),
       techniqueId = Value(techniqueId),
       techniqueNameSnapshot = Value(techniqueNameSnapshot);
  static Insertable<TechniqueMarkerRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? trackId,
    Expression<int>? positionMs,
    Expression<int>? endMs,
    Expression<String>? techniqueId,
    Expression<String>? techniqueNameSnapshot,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (trackId != null) 'track_id': trackId,
      if (positionMs != null) 'position_ms': positionMs,
      if (endMs != null) 'end_ms': endMs,
      if (techniqueId != null) 'technique_id': techniqueId,
      if (techniqueNameSnapshot != null)
        'technique_name_snapshot': techniqueNameSnapshot,
      if (rowid != null) 'rowid': rowid,
    });
  }

  TechniqueMarkersCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? trackId,
    Value<int>? positionMs,
    Value<int?>? endMs,
    Value<String>? techniqueId,
    Value<String>? techniqueNameSnapshot,
    Value<int>? rowid,
  }) {
    return TechniqueMarkersCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      trackId: trackId ?? this.trackId,
      positionMs: positionMs ?? this.positionMs,
      endMs: endMs ?? this.endMs,
      techniqueId: techniqueId ?? this.techniqueId,
      techniqueNameSnapshot:
          techniqueNameSnapshot ?? this.techniqueNameSnapshot,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (trackId.present) {
      map['track_id'] = Variable<String>(trackId.value);
    }
    if (positionMs.present) {
      map['position_ms'] = Variable<int>(positionMs.value);
    }
    if (endMs.present) {
      map['end_ms'] = Variable<int>(endMs.value);
    }
    if (techniqueId.present) {
      map['technique_id'] = Variable<String>(techniqueId.value);
    }
    if (techniqueNameSnapshot.present) {
      map['technique_name_snapshot'] = Variable<String>(
        techniqueNameSnapshot.value,
      );
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('TechniqueMarkersCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('trackId: $trackId, ')
          ..write('positionMs: $positionMs, ')
          ..write('endMs: $endMs, ')
          ..write('techniqueId: $techniqueId, ')
          ..write('techniqueNameSnapshot: $techniqueNameSnapshot, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $TechniquesTable extends Techniques
    with TableInfo<$TechniquesTable, TechniqueRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $TechniquesTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _nameMeta = const VerificationMeta('name');
  @override
  late final GeneratedColumn<String> name = GeneratedColumn<String>(
    'name',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _aliasesJsonMeta = const VerificationMeta(
    'aliasesJson',
  );
  @override
  late final GeneratedColumn<String> aliasesJson = GeneratedColumn<String>(
    'aliases_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _tagsJsonMeta = const VerificationMeta(
    'tagsJson',
  );
  @override
  late final GeneratedColumn<String> tagsJson = GeneratedColumn<String>(
    'tags_json',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _summaryMeta = const VerificationMeta(
    'summary',
  );
  @override
  late final GeneratedColumn<String> summary = GeneratedColumn<String>(
    'summary',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _descriptionMeta = const VerificationMeta(
    'description',
  );
  @override
  late final GeneratedColumn<String> description = GeneratedColumn<String>(
    'description',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _cuesMeta = const VerificationMeta('cues');
  @override
  late final GeneratedColumn<String> cues = GeneratedColumn<String>(
    'cues',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _exercisesMeta = const VerificationMeta(
    'exercises',
  );
  @override
  late final GeneratedColumn<String> exercises = GeneratedColumn<String>(
    'exercises',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _acousticSignatureMeta = const VerificationMeta(
    'acousticSignature',
  );
  @override
  late final GeneratedColumn<String> acousticSignature =
      GeneratedColumn<String>(
        'acoustic_signature',
        aliasedName,
        false,
        type: DriftSqlType.string,
        requiredDuringInsert: false,
        defaultValue: const Constant(''),
      );
  static const VerificationMeta _orderIndexMeta = const VerificationMeta(
    'orderIndex',
  );
  @override
  late final GeneratedColumn<int> orderIndex = GeneratedColumn<int>(
    'order_index',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    name,
    aliasesJson,
    tagsJson,
    summary,
    description,
    cues,
    exercises,
    acousticSignature,
    orderIndex,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'techniques';
  @override
  VerificationContext validateIntegrity(
    Insertable<TechniqueRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('name')) {
      context.handle(
        _nameMeta,
        name.isAcceptableOrUnknown(data['name']!, _nameMeta),
      );
    }
    if (data.containsKey('aliases_json')) {
      context.handle(
        _aliasesJsonMeta,
        aliasesJson.isAcceptableOrUnknown(
          data['aliases_json']!,
          _aliasesJsonMeta,
        ),
      );
    }
    if (data.containsKey('tags_json')) {
      context.handle(
        _tagsJsonMeta,
        tagsJson.isAcceptableOrUnknown(data['tags_json']!, _tagsJsonMeta),
      );
    }
    if (data.containsKey('summary')) {
      context.handle(
        _summaryMeta,
        summary.isAcceptableOrUnknown(data['summary']!, _summaryMeta),
      );
    }
    if (data.containsKey('description')) {
      context.handle(
        _descriptionMeta,
        description.isAcceptableOrUnknown(
          data['description']!,
          _descriptionMeta,
        ),
      );
    }
    if (data.containsKey('cues')) {
      context.handle(
        _cuesMeta,
        cues.isAcceptableOrUnknown(data['cues']!, _cuesMeta),
      );
    }
    if (data.containsKey('exercises')) {
      context.handle(
        _exercisesMeta,
        exercises.isAcceptableOrUnknown(data['exercises']!, _exercisesMeta),
      );
    }
    if (data.containsKey('acoustic_signature')) {
      context.handle(
        _acousticSignatureMeta,
        acousticSignature.isAcceptableOrUnknown(
          data['acoustic_signature']!,
          _acousticSignatureMeta,
        ),
      );
    }
    if (data.containsKey('order_index')) {
      context.handle(
        _orderIndexMeta,
        orderIndex.isAcceptableOrUnknown(data['order_index']!, _orderIndexMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  TechniqueRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return TechniqueRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      name: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}name'],
      )!,
      aliasesJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}aliases_json'],
      ),
      tagsJson: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}tags_json'],
      ),
      summary: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}summary'],
      )!,
      description: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}description'],
      )!,
      cues: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}cues'],
      )!,
      exercises: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}exercises'],
      )!,
      acousticSignature: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}acoustic_signature'],
      )!,
      orderIndex: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}order_index'],
      )!,
    );
  }

  @override
  $TechniquesTable createAlias(String alias) {
    return $TechniquesTable(attachedDatabase, alias);
  }
}

class TechniqueRow extends DataClass implements Insertable<TechniqueRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String name;
  final String? aliasesJson;
  final String? tagsJson;
  final String summary;
  final String description;
  final String cues;
  final String exercises;
  final String acousticSignature;
  final int orderIndex;
  const TechniqueRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.name,
    this.aliasesJson,
    this.tagsJson,
    required this.summary,
    required this.description,
    required this.cues,
    required this.exercises,
    required this.acousticSignature,
    required this.orderIndex,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['name'] = Variable<String>(name);
    if (!nullToAbsent || aliasesJson != null) {
      map['aliases_json'] = Variable<String>(aliasesJson);
    }
    if (!nullToAbsent || tagsJson != null) {
      map['tags_json'] = Variable<String>(tagsJson);
    }
    map['summary'] = Variable<String>(summary);
    map['description'] = Variable<String>(description);
    map['cues'] = Variable<String>(cues);
    map['exercises'] = Variable<String>(exercises);
    map['acoustic_signature'] = Variable<String>(acousticSignature);
    map['order_index'] = Variable<int>(orderIndex);
    return map;
  }

  TechniquesCompanion toCompanion(bool nullToAbsent) {
    return TechniquesCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      name: Value(name),
      aliasesJson: aliasesJson == null && nullToAbsent
          ? const Value.absent()
          : Value(aliasesJson),
      tagsJson: tagsJson == null && nullToAbsent
          ? const Value.absent()
          : Value(tagsJson),
      summary: Value(summary),
      description: Value(description),
      cues: Value(cues),
      exercises: Value(exercises),
      acousticSignature: Value(acousticSignature),
      orderIndex: Value(orderIndex),
    );
  }

  factory TechniqueRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return TechniqueRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      name: serializer.fromJson<String>(json['name']),
      aliasesJson: serializer.fromJson<String?>(json['aliasesJson']),
      tagsJson: serializer.fromJson<String?>(json['tagsJson']),
      summary: serializer.fromJson<String>(json['summary']),
      description: serializer.fromJson<String>(json['description']),
      cues: serializer.fromJson<String>(json['cues']),
      exercises: serializer.fromJson<String>(json['exercises']),
      acousticSignature: serializer.fromJson<String>(json['acousticSignature']),
      orderIndex: serializer.fromJson<int>(json['orderIndex']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'name': serializer.toJson<String>(name),
      'aliasesJson': serializer.toJson<String?>(aliasesJson),
      'tagsJson': serializer.toJson<String?>(tagsJson),
      'summary': serializer.toJson<String>(summary),
      'description': serializer.toJson<String>(description),
      'cues': serializer.toJson<String>(cues),
      'exercises': serializer.toJson<String>(exercises),
      'acousticSignature': serializer.toJson<String>(acousticSignature),
      'orderIndex': serializer.toJson<int>(orderIndex),
    };
  }

  TechniqueRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? name,
    Value<String?> aliasesJson = const Value.absent(),
    Value<String?> tagsJson = const Value.absent(),
    String? summary,
    String? description,
    String? cues,
    String? exercises,
    String? acousticSignature,
    int? orderIndex,
  }) => TechniqueRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    name: name ?? this.name,
    aliasesJson: aliasesJson.present ? aliasesJson.value : this.aliasesJson,
    tagsJson: tagsJson.present ? tagsJson.value : this.tagsJson,
    summary: summary ?? this.summary,
    description: description ?? this.description,
    cues: cues ?? this.cues,
    exercises: exercises ?? this.exercises,
    acousticSignature: acousticSignature ?? this.acousticSignature,
    orderIndex: orderIndex ?? this.orderIndex,
  );
  TechniqueRow copyWithCompanion(TechniquesCompanion data) {
    return TechniqueRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      name: data.name.present ? data.name.value : this.name,
      aliasesJson: data.aliasesJson.present
          ? data.aliasesJson.value
          : this.aliasesJson,
      tagsJson: data.tagsJson.present ? data.tagsJson.value : this.tagsJson,
      summary: data.summary.present ? data.summary.value : this.summary,
      description: data.description.present
          ? data.description.value
          : this.description,
      cues: data.cues.present ? data.cues.value : this.cues,
      exercises: data.exercises.present ? data.exercises.value : this.exercises,
      acousticSignature: data.acousticSignature.present
          ? data.acousticSignature.value
          : this.acousticSignature,
      orderIndex: data.orderIndex.present
          ? data.orderIndex.value
          : this.orderIndex,
    );
  }

  @override
  String toString() {
    return (StringBuffer('TechniqueRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('name: $name, ')
          ..write('aliasesJson: $aliasesJson, ')
          ..write('tagsJson: $tagsJson, ')
          ..write('summary: $summary, ')
          ..write('description: $description, ')
          ..write('cues: $cues, ')
          ..write('exercises: $exercises, ')
          ..write('acousticSignature: $acousticSignature, ')
          ..write('orderIndex: $orderIndex')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    name,
    aliasesJson,
    tagsJson,
    summary,
    description,
    cues,
    exercises,
    acousticSignature,
    orderIndex,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is TechniqueRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.name == this.name &&
          other.aliasesJson == this.aliasesJson &&
          other.tagsJson == this.tagsJson &&
          other.summary == this.summary &&
          other.description == this.description &&
          other.cues == this.cues &&
          other.exercises == this.exercises &&
          other.acousticSignature == this.acousticSignature &&
          other.orderIndex == this.orderIndex);
}

class TechniquesCompanion extends UpdateCompanion<TechniqueRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> name;
  final Value<String?> aliasesJson;
  final Value<String?> tagsJson;
  final Value<String> summary;
  final Value<String> description;
  final Value<String> cues;
  final Value<String> exercises;
  final Value<String> acousticSignature;
  final Value<int> orderIndex;
  final Value<int> rowid;
  const TechniquesCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.name = const Value.absent(),
    this.aliasesJson = const Value.absent(),
    this.tagsJson = const Value.absent(),
    this.summary = const Value.absent(),
    this.description = const Value.absent(),
    this.cues = const Value.absent(),
    this.exercises = const Value.absent(),
    this.acousticSignature = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  TechniquesCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    this.name = const Value.absent(),
    this.aliasesJson = const Value.absent(),
    this.tagsJson = const Value.absent(),
    this.summary = const Value.absent(),
    this.description = const Value.absent(),
    this.cues = const Value.absent(),
    this.exercises = const Value.absent(),
    this.acousticSignature = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked);
  static Insertable<TechniqueRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? name,
    Expression<String>? aliasesJson,
    Expression<String>? tagsJson,
    Expression<String>? summary,
    Expression<String>? description,
    Expression<String>? cues,
    Expression<String>? exercises,
    Expression<String>? acousticSignature,
    Expression<int>? orderIndex,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (name != null) 'name': name,
      if (aliasesJson != null) 'aliases_json': aliasesJson,
      if (tagsJson != null) 'tags_json': tagsJson,
      if (summary != null) 'summary': summary,
      if (description != null) 'description': description,
      if (cues != null) 'cues': cues,
      if (exercises != null) 'exercises': exercises,
      if (acousticSignature != null) 'acoustic_signature': acousticSignature,
      if (orderIndex != null) 'order_index': orderIndex,
      if (rowid != null) 'rowid': rowid,
    });
  }

  TechniquesCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? name,
    Value<String?>? aliasesJson,
    Value<String?>? tagsJson,
    Value<String>? summary,
    Value<String>? description,
    Value<String>? cues,
    Value<String>? exercises,
    Value<String>? acousticSignature,
    Value<int>? orderIndex,
    Value<int>? rowid,
  }) {
    return TechniquesCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      name: name ?? this.name,
      aliasesJson: aliasesJson ?? this.aliasesJson,
      tagsJson: tagsJson ?? this.tagsJson,
      summary: summary ?? this.summary,
      description: description ?? this.description,
      cues: cues ?? this.cues,
      exercises: exercises ?? this.exercises,
      acousticSignature: acousticSignature ?? this.acousticSignature,
      orderIndex: orderIndex ?? this.orderIndex,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (name.present) {
      map['name'] = Variable<String>(name.value);
    }
    if (aliasesJson.present) {
      map['aliases_json'] = Variable<String>(aliasesJson.value);
    }
    if (tagsJson.present) {
      map['tags_json'] = Variable<String>(tagsJson.value);
    }
    if (summary.present) {
      map['summary'] = Variable<String>(summary.value);
    }
    if (description.present) {
      map['description'] = Variable<String>(description.value);
    }
    if (cues.present) {
      map['cues'] = Variable<String>(cues.value);
    }
    if (exercises.present) {
      map['exercises'] = Variable<String>(exercises.value);
    }
    if (acousticSignature.present) {
      map['acoustic_signature'] = Variable<String>(acousticSignature.value);
    }
    if (orderIndex.present) {
      map['order_index'] = Variable<int>(orderIndex.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('TechniquesCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('name: $name, ')
          ..write('aliasesJson: $aliasesJson, ')
          ..write('tagsJson: $tagsJson, ')
          ..write('summary: $summary, ')
          ..write('description: $description, ')
          ..write('cues: $cues, ')
          ..write('exercises: $exercises, ')
          ..write('acousticSignature: $acousticSignature, ')
          ..write('orderIndex: $orderIndex, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $TechniqueMediaTable extends TechniqueMedia
    with TableInfo<$TechniqueMediaTable, TechniqueMediaRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $TechniqueMediaTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    additionalChecks: GeneratedColumn.checkTextLength(
      minTextLength: 26,
      maxTextLength: 36,
    ),
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _updatedAtMeta = const VerificationMeta(
    'updatedAt',
  );
  @override
  late final GeneratedColumn<int> updatedAt = GeneratedColumn<int>(
    'updated_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _deletedAtMeta = const VerificationMeta(
    'deletedAt',
  );
  @override
  late final GeneratedColumn<int> deletedAt = GeneratedColumn<int>(
    'deleted_at',
    aliasedName,
    true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _hlcWallMeta = const VerificationMeta(
    'hlcWall',
  );
  @override
  late final GeneratedColumn<int> hlcWall = GeneratedColumn<int>(
    'hlc_wall',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcCtrMeta = const VerificationMeta('hlcCtr');
  @override
  late final GeneratedColumn<int> hlcCtr = GeneratedColumn<int>(
    'hlc_ctr',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcNodeMeta = const VerificationMeta(
    'hlcNode',
  );
  @override
  late final GeneratedColumn<String> hlcNode = GeneratedColumn<String>(
    'hlc_node',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _schemaVMeta = const VerificationMeta(
    'schemaV',
  );
  @override
  late final GeneratedColumn<int> schemaV = GeneratedColumn<int>(
    'schema_v',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(1),
  );
  static const VerificationMeta _techniqueIdMeta = const VerificationMeta(
    'techniqueId',
  );
  @override
  late final GeneratedColumn<String> techniqueId = GeneratedColumn<String>(
    'technique_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES techniques (id)',
    ),
  );
  static const VerificationMeta _assetIdMeta = const VerificationMeta(
    'assetId',
  );
  @override
  late final GeneratedColumn<String> assetId = GeneratedColumn<String>(
    'asset_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'REFERENCES assets (id)',
    ),
  );
  static const VerificationMeta _kindMeta = const VerificationMeta('kind');
  @override
  late final GeneratedColumn<String> kind = GeneratedColumn<String>(
    'kind',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant('audio'),
  );
  static const VerificationMeta _labelMeta = const VerificationMeta('label');
  @override
  late final GeneratedColumn<String> label = GeneratedColumn<String>(
    'label',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
    defaultValue: const Constant(''),
  );
  static const VerificationMeta _orderIndexMeta = const VerificationMeta(
    'orderIndex',
  );
  @override
  late final GeneratedColumn<int> orderIndex = GeneratedColumn<int>(
    'order_index',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultValue: const Constant(0),
  );
  @override
  List<GeneratedColumn> get $columns => [
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    techniqueId,
    assetId,
    kind,
    label,
    orderIndex,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'technique_media';
  @override
  VerificationContext validateIntegrity(
    Insertable<TechniqueMediaRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    if (data.containsKey('updated_at')) {
      context.handle(
        _updatedAtMeta,
        updatedAt.isAcceptableOrUnknown(data['updated_at']!, _updatedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_updatedAtMeta);
    }
    if (data.containsKey('deleted_at')) {
      context.handle(
        _deletedAtMeta,
        deletedAt.isAcceptableOrUnknown(data['deleted_at']!, _deletedAtMeta),
      );
    }
    if (data.containsKey('hlc_wall')) {
      context.handle(
        _hlcWallMeta,
        hlcWall.isAcceptableOrUnknown(data['hlc_wall']!, _hlcWallMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcWallMeta);
    }
    if (data.containsKey('hlc_ctr')) {
      context.handle(
        _hlcCtrMeta,
        hlcCtr.isAcceptableOrUnknown(data['hlc_ctr']!, _hlcCtrMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcCtrMeta);
    }
    if (data.containsKey('hlc_node')) {
      context.handle(
        _hlcNodeMeta,
        hlcNode.isAcceptableOrUnknown(data['hlc_node']!, _hlcNodeMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcNodeMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('schema_v')) {
      context.handle(
        _schemaVMeta,
        schemaV.isAcceptableOrUnknown(data['schema_v']!, _schemaVMeta),
      );
    }
    if (data.containsKey('technique_id')) {
      context.handle(
        _techniqueIdMeta,
        techniqueId.isAcceptableOrUnknown(
          data['technique_id']!,
          _techniqueIdMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_techniqueIdMeta);
    }
    if (data.containsKey('asset_id')) {
      context.handle(
        _assetIdMeta,
        assetId.isAcceptableOrUnknown(data['asset_id']!, _assetIdMeta),
      );
    } else if (isInserting) {
      context.missing(_assetIdMeta);
    }
    if (data.containsKey('kind')) {
      context.handle(
        _kindMeta,
        kind.isAcceptableOrUnknown(data['kind']!, _kindMeta),
      );
    }
    if (data.containsKey('label')) {
      context.handle(
        _labelMeta,
        label.isAcceptableOrUnknown(data['label']!, _labelMeta),
      );
    }
    if (data.containsKey('order_index')) {
      context.handle(
        _orderIndexMeta,
        orderIndex.isAcceptableOrUnknown(data['order_index']!, _orderIndexMeta),
      );
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  TechniqueMediaRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return TechniqueMediaRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
      updatedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}updated_at'],
      )!,
      deletedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}deleted_at'],
      ),
      hlcWall: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_wall'],
      )!,
      hlcCtr: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}hlc_ctr'],
      )!,
      hlcNode: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_node'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      schemaV: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}schema_v'],
      )!,
      techniqueId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}technique_id'],
      )!,
      assetId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}asset_id'],
      )!,
      kind: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}kind'],
      )!,
      label: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}label'],
      )!,
      orderIndex: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}order_index'],
      )!,
    );
  }

  @override
  $TechniqueMediaTable createAlias(String alias) {
    return $TechniqueMediaTable(attachedDatabase, alias);
  }
}

class TechniqueMediaRow extends DataClass
    implements Insertable<TechniqueMediaRow> {
  /// Stable ULID primary key; doubles as the CloudKit record name.
  final String id;

  /// Creation / last-modification wall time (epoch ms).
  final int createdAt;
  final int updatedAt;

  /// Tombstone: NULL = live. Reads filter `deleted_at IS NULL`; deletes set it.
  final int? deletedAt;

  /// Hybrid Logical Clock triple (the LWW key). hlcPacked is the fixed-width
  /// lexicographic encoding stored for a one-comparison conflict check.
  final int hlcWall;
  final int hlcCtr;
  final String hlcNode;
  final String hlcPacked;

  /// The app/record schema version that produced this row (cross-device skew
  /// gate + JSON serializer version).
  final int schemaV;
  final String techniqueId;
  final String assetId;
  final String kind;
  final String label;
  final int orderIndex;
  const TechniqueMediaRow({
    required this.id,
    required this.createdAt,
    required this.updatedAt,
    this.deletedAt,
    required this.hlcWall,
    required this.hlcCtr,
    required this.hlcNode,
    required this.hlcPacked,
    required this.schemaV,
    required this.techniqueId,
    required this.assetId,
    required this.kind,
    required this.label,
    required this.orderIndex,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['created_at'] = Variable<int>(createdAt);
    map['updated_at'] = Variable<int>(updatedAt);
    if (!nullToAbsent || deletedAt != null) {
      map['deleted_at'] = Variable<int>(deletedAt);
    }
    map['hlc_wall'] = Variable<int>(hlcWall);
    map['hlc_ctr'] = Variable<int>(hlcCtr);
    map['hlc_node'] = Variable<String>(hlcNode);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['schema_v'] = Variable<int>(schemaV);
    map['technique_id'] = Variable<String>(techniqueId);
    map['asset_id'] = Variable<String>(assetId);
    map['kind'] = Variable<String>(kind);
    map['label'] = Variable<String>(label);
    map['order_index'] = Variable<int>(orderIndex);
    return map;
  }

  TechniqueMediaCompanion toCompanion(bool nullToAbsent) {
    return TechniqueMediaCompanion(
      id: Value(id),
      createdAt: Value(createdAt),
      updatedAt: Value(updatedAt),
      deletedAt: deletedAt == null && nullToAbsent
          ? const Value.absent()
          : Value(deletedAt),
      hlcWall: Value(hlcWall),
      hlcCtr: Value(hlcCtr),
      hlcNode: Value(hlcNode),
      hlcPacked: Value(hlcPacked),
      schemaV: Value(schemaV),
      techniqueId: Value(techniqueId),
      assetId: Value(assetId),
      kind: Value(kind),
      label: Value(label),
      orderIndex: Value(orderIndex),
    );
  }

  factory TechniqueMediaRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return TechniqueMediaRow(
      id: serializer.fromJson<String>(json['id']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
      updatedAt: serializer.fromJson<int>(json['updatedAt']),
      deletedAt: serializer.fromJson<int?>(json['deletedAt']),
      hlcWall: serializer.fromJson<int>(json['hlcWall']),
      hlcCtr: serializer.fromJson<int>(json['hlcCtr']),
      hlcNode: serializer.fromJson<String>(json['hlcNode']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      schemaV: serializer.fromJson<int>(json['schemaV']),
      techniqueId: serializer.fromJson<String>(json['techniqueId']),
      assetId: serializer.fromJson<String>(json['assetId']),
      kind: serializer.fromJson<String>(json['kind']),
      label: serializer.fromJson<String>(json['label']),
      orderIndex: serializer.fromJson<int>(json['orderIndex']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'createdAt': serializer.toJson<int>(createdAt),
      'updatedAt': serializer.toJson<int>(updatedAt),
      'deletedAt': serializer.toJson<int?>(deletedAt),
      'hlcWall': serializer.toJson<int>(hlcWall),
      'hlcCtr': serializer.toJson<int>(hlcCtr),
      'hlcNode': serializer.toJson<String>(hlcNode),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'schemaV': serializer.toJson<int>(schemaV),
      'techniqueId': serializer.toJson<String>(techniqueId),
      'assetId': serializer.toJson<String>(assetId),
      'kind': serializer.toJson<String>(kind),
      'label': serializer.toJson<String>(label),
      'orderIndex': serializer.toJson<int>(orderIndex),
    };
  }

  TechniqueMediaRow copyWith({
    String? id,
    int? createdAt,
    int? updatedAt,
    Value<int?> deletedAt = const Value.absent(),
    int? hlcWall,
    int? hlcCtr,
    String? hlcNode,
    String? hlcPacked,
    int? schemaV,
    String? techniqueId,
    String? assetId,
    String? kind,
    String? label,
    int? orderIndex,
  }) => TechniqueMediaRow(
    id: id ?? this.id,
    createdAt: createdAt ?? this.createdAt,
    updatedAt: updatedAt ?? this.updatedAt,
    deletedAt: deletedAt.present ? deletedAt.value : this.deletedAt,
    hlcWall: hlcWall ?? this.hlcWall,
    hlcCtr: hlcCtr ?? this.hlcCtr,
    hlcNode: hlcNode ?? this.hlcNode,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    schemaV: schemaV ?? this.schemaV,
    techniqueId: techniqueId ?? this.techniqueId,
    assetId: assetId ?? this.assetId,
    kind: kind ?? this.kind,
    label: label ?? this.label,
    orderIndex: orderIndex ?? this.orderIndex,
  );
  TechniqueMediaRow copyWithCompanion(TechniqueMediaCompanion data) {
    return TechniqueMediaRow(
      id: data.id.present ? data.id.value : this.id,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
      updatedAt: data.updatedAt.present ? data.updatedAt.value : this.updatedAt,
      deletedAt: data.deletedAt.present ? data.deletedAt.value : this.deletedAt,
      hlcWall: data.hlcWall.present ? data.hlcWall.value : this.hlcWall,
      hlcCtr: data.hlcCtr.present ? data.hlcCtr.value : this.hlcCtr,
      hlcNode: data.hlcNode.present ? data.hlcNode.value : this.hlcNode,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      schemaV: data.schemaV.present ? data.schemaV.value : this.schemaV,
      techniqueId: data.techniqueId.present
          ? data.techniqueId.value
          : this.techniqueId,
      assetId: data.assetId.present ? data.assetId.value : this.assetId,
      kind: data.kind.present ? data.kind.value : this.kind,
      label: data.label.present ? data.label.value : this.label,
      orderIndex: data.orderIndex.present
          ? data.orderIndex.value
          : this.orderIndex,
    );
  }

  @override
  String toString() {
    return (StringBuffer('TechniqueMediaRow(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('techniqueId: $techniqueId, ')
          ..write('assetId: $assetId, ')
          ..write('kind: $kind, ')
          ..write('label: $label, ')
          ..write('orderIndex: $orderIndex')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    id,
    createdAt,
    updatedAt,
    deletedAt,
    hlcWall,
    hlcCtr,
    hlcNode,
    hlcPacked,
    schemaV,
    techniqueId,
    assetId,
    kind,
    label,
    orderIndex,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is TechniqueMediaRow &&
          other.id == this.id &&
          other.createdAt == this.createdAt &&
          other.updatedAt == this.updatedAt &&
          other.deletedAt == this.deletedAt &&
          other.hlcWall == this.hlcWall &&
          other.hlcCtr == this.hlcCtr &&
          other.hlcNode == this.hlcNode &&
          other.hlcPacked == this.hlcPacked &&
          other.schemaV == this.schemaV &&
          other.techniqueId == this.techniqueId &&
          other.assetId == this.assetId &&
          other.kind == this.kind &&
          other.label == this.label &&
          other.orderIndex == this.orderIndex);
}

class TechniqueMediaCompanion extends UpdateCompanion<TechniqueMediaRow> {
  final Value<String> id;
  final Value<int> createdAt;
  final Value<int> updatedAt;
  final Value<int?> deletedAt;
  final Value<int> hlcWall;
  final Value<int> hlcCtr;
  final Value<String> hlcNode;
  final Value<String> hlcPacked;
  final Value<int> schemaV;
  final Value<String> techniqueId;
  final Value<String> assetId;
  final Value<String> kind;
  final Value<String> label;
  final Value<int> orderIndex;
  final Value<int> rowid;
  const TechniqueMediaCompanion({
    this.id = const Value.absent(),
    this.createdAt = const Value.absent(),
    this.updatedAt = const Value.absent(),
    this.deletedAt = const Value.absent(),
    this.hlcWall = const Value.absent(),
    this.hlcCtr = const Value.absent(),
    this.hlcNode = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.schemaV = const Value.absent(),
    this.techniqueId = const Value.absent(),
    this.assetId = const Value.absent(),
    this.kind = const Value.absent(),
    this.label = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  TechniqueMediaCompanion.insert({
    required String id,
    required int createdAt,
    required int updatedAt,
    this.deletedAt = const Value.absent(),
    required int hlcWall,
    required int hlcCtr,
    required String hlcNode,
    required String hlcPacked,
    this.schemaV = const Value.absent(),
    required String techniqueId,
    required String assetId,
    this.kind = const Value.absent(),
    this.label = const Value.absent(),
    this.orderIndex = const Value.absent(),
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       createdAt = Value(createdAt),
       updatedAt = Value(updatedAt),
       hlcWall = Value(hlcWall),
       hlcCtr = Value(hlcCtr),
       hlcNode = Value(hlcNode),
       hlcPacked = Value(hlcPacked),
       techniqueId = Value(techniqueId),
       assetId = Value(assetId);
  static Insertable<TechniqueMediaRow> custom({
    Expression<String>? id,
    Expression<int>? createdAt,
    Expression<int>? updatedAt,
    Expression<int>? deletedAt,
    Expression<int>? hlcWall,
    Expression<int>? hlcCtr,
    Expression<String>? hlcNode,
    Expression<String>? hlcPacked,
    Expression<int>? schemaV,
    Expression<String>? techniqueId,
    Expression<String>? assetId,
    Expression<String>? kind,
    Expression<String>? label,
    Expression<int>? orderIndex,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (createdAt != null) 'created_at': createdAt,
      if (updatedAt != null) 'updated_at': updatedAt,
      if (deletedAt != null) 'deleted_at': deletedAt,
      if (hlcWall != null) 'hlc_wall': hlcWall,
      if (hlcCtr != null) 'hlc_ctr': hlcCtr,
      if (hlcNode != null) 'hlc_node': hlcNode,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (schemaV != null) 'schema_v': schemaV,
      if (techniqueId != null) 'technique_id': techniqueId,
      if (assetId != null) 'asset_id': assetId,
      if (kind != null) 'kind': kind,
      if (label != null) 'label': label,
      if (orderIndex != null) 'order_index': orderIndex,
      if (rowid != null) 'rowid': rowid,
    });
  }

  TechniqueMediaCompanion copyWith({
    Value<String>? id,
    Value<int>? createdAt,
    Value<int>? updatedAt,
    Value<int?>? deletedAt,
    Value<int>? hlcWall,
    Value<int>? hlcCtr,
    Value<String>? hlcNode,
    Value<String>? hlcPacked,
    Value<int>? schemaV,
    Value<String>? techniqueId,
    Value<String>? assetId,
    Value<String>? kind,
    Value<String>? label,
    Value<int>? orderIndex,
    Value<int>? rowid,
  }) {
    return TechniqueMediaCompanion(
      id: id ?? this.id,
      createdAt: createdAt ?? this.createdAt,
      updatedAt: updatedAt ?? this.updatedAt,
      deletedAt: deletedAt ?? this.deletedAt,
      hlcWall: hlcWall ?? this.hlcWall,
      hlcCtr: hlcCtr ?? this.hlcCtr,
      hlcNode: hlcNode ?? this.hlcNode,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      schemaV: schemaV ?? this.schemaV,
      techniqueId: techniqueId ?? this.techniqueId,
      assetId: assetId ?? this.assetId,
      kind: kind ?? this.kind,
      label: label ?? this.label,
      orderIndex: orderIndex ?? this.orderIndex,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    if (updatedAt.present) {
      map['updated_at'] = Variable<int>(updatedAt.value);
    }
    if (deletedAt.present) {
      map['deleted_at'] = Variable<int>(deletedAt.value);
    }
    if (hlcWall.present) {
      map['hlc_wall'] = Variable<int>(hlcWall.value);
    }
    if (hlcCtr.present) {
      map['hlc_ctr'] = Variable<int>(hlcCtr.value);
    }
    if (hlcNode.present) {
      map['hlc_node'] = Variable<String>(hlcNode.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (schemaV.present) {
      map['schema_v'] = Variable<int>(schemaV.value);
    }
    if (techniqueId.present) {
      map['technique_id'] = Variable<String>(techniqueId.value);
    }
    if (assetId.present) {
      map['asset_id'] = Variable<String>(assetId.value);
    }
    if (kind.present) {
      map['kind'] = Variable<String>(kind.value);
    }
    if (label.present) {
      map['label'] = Variable<String>(label.value);
    }
    if (orderIndex.present) {
      map['order_index'] = Variable<int>(orderIndex.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('TechniqueMediaCompanion(')
          ..write('id: $id, ')
          ..write('createdAt: $createdAt, ')
          ..write('updatedAt: $updatedAt, ')
          ..write('deletedAt: $deletedAt, ')
          ..write('hlcWall: $hlcWall, ')
          ..write('hlcCtr: $hlcCtr, ')
          ..write('hlcNode: $hlcNode, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('schemaV: $schemaV, ')
          ..write('techniqueId: $techniqueId, ')
          ..write('assetId: $assetId, ')
          ..write('kind: $kind, ')
          ..write('label: $label, ')
          ..write('orderIndex: $orderIndex, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $OplogTable extends Oplog with TableInfo<$OplogTable, OplogRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $OplogTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _seqMeta = const VerificationMeta('seq');
  @override
  late final GeneratedColumn<int> seq = GeneratedColumn<int>(
    'seq',
    aliasedName,
    false,
    hasAutoIncrement: true,
    type: DriftSqlType.int,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'PRIMARY KEY AUTOINCREMENT',
    ),
  );
  static const VerificationMeta _entityTableMeta = const VerificationMeta(
    'entityTable',
  );
  @override
  late final GeneratedColumn<String> entityTable = GeneratedColumn<String>(
    'entity_table',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _entityIdMeta = const VerificationMeta(
    'entityId',
  );
  @override
  late final GeneratedColumn<String> entityId = GeneratedColumn<String>(
    'entity_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _opMeta = const VerificationMeta('op');
  @override
  late final GeneratedColumn<String> op = GeneratedColumn<String>(
    'op',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _hlcPackedMeta = const VerificationMeta(
    'hlcPacked',
  );
  @override
  late final GeneratedColumn<String> hlcPacked = GeneratedColumn<String>(
    'hlc_packed',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _payloadHashMeta = const VerificationMeta(
    'payloadHash',
  );
  @override
  late final GeneratedColumn<String> payloadHash = GeneratedColumn<String>(
    'payload_hash',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _syncedMeta = const VerificationMeta('synced');
  @override
  late final GeneratedColumn<bool> synced = GeneratedColumn<bool>(
    'synced',
    aliasedName,
    false,
    type: DriftSqlType.bool,
    requiredDuringInsert: false,
    defaultConstraints: GeneratedColumn.constraintIsAlways(
      'CHECK ("synced" IN (0, 1))',
    ),
    defaultValue: const Constant(false),
  );
  static const VerificationMeta _dependsOnAssetIdMeta = const VerificationMeta(
    'dependsOnAssetId',
  );
  @override
  late final GeneratedColumn<String> dependsOnAssetId = GeneratedColumn<String>(
    'depends_on_asset_id',
    aliasedName,
    true,
    type: DriftSqlType.string,
    requiredDuringInsert: false,
  );
  static const VerificationMeta _createdAtMeta = const VerificationMeta(
    'createdAt',
  );
  @override
  late final GeneratedColumn<int> createdAt = GeneratedColumn<int>(
    'created_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  @override
  List<GeneratedColumn> get $columns => [
    seq,
    entityTable,
    entityId,
    op,
    hlcPacked,
    payloadHash,
    synced,
    dependsOnAssetId,
    createdAt,
  ];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'oplog';
  @override
  VerificationContext validateIntegrity(
    Insertable<OplogRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('seq')) {
      context.handle(
        _seqMeta,
        seq.isAcceptableOrUnknown(data['seq']!, _seqMeta),
      );
    }
    if (data.containsKey('entity_table')) {
      context.handle(
        _entityTableMeta,
        entityTable.isAcceptableOrUnknown(
          data['entity_table']!,
          _entityTableMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_entityTableMeta);
    }
    if (data.containsKey('entity_id')) {
      context.handle(
        _entityIdMeta,
        entityId.isAcceptableOrUnknown(data['entity_id']!, _entityIdMeta),
      );
    } else if (isInserting) {
      context.missing(_entityIdMeta);
    }
    if (data.containsKey('op')) {
      context.handle(_opMeta, op.isAcceptableOrUnknown(data['op']!, _opMeta));
    } else if (isInserting) {
      context.missing(_opMeta);
    }
    if (data.containsKey('hlc_packed')) {
      context.handle(
        _hlcPackedMeta,
        hlcPacked.isAcceptableOrUnknown(data['hlc_packed']!, _hlcPackedMeta),
      );
    } else if (isInserting) {
      context.missing(_hlcPackedMeta);
    }
    if (data.containsKey('payload_hash')) {
      context.handle(
        _payloadHashMeta,
        payloadHash.isAcceptableOrUnknown(
          data['payload_hash']!,
          _payloadHashMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_payloadHashMeta);
    }
    if (data.containsKey('synced')) {
      context.handle(
        _syncedMeta,
        synced.isAcceptableOrUnknown(data['synced']!, _syncedMeta),
      );
    }
    if (data.containsKey('depends_on_asset_id')) {
      context.handle(
        _dependsOnAssetIdMeta,
        dependsOnAssetId.isAcceptableOrUnknown(
          data['depends_on_asset_id']!,
          _dependsOnAssetIdMeta,
        ),
      );
    }
    if (data.containsKey('created_at')) {
      context.handle(
        _createdAtMeta,
        createdAt.isAcceptableOrUnknown(data['created_at']!, _createdAtMeta),
      );
    } else if (isInserting) {
      context.missing(_createdAtMeta);
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {seq};
  @override
  OplogRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return OplogRow(
      seq: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}seq'],
      )!,
      entityTable: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}entity_table'],
      )!,
      entityId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}entity_id'],
      )!,
      op: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}op'],
      )!,
      hlcPacked: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}hlc_packed'],
      )!,
      payloadHash: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}payload_hash'],
      )!,
      synced: attachedDatabase.typeMapping.read(
        DriftSqlType.bool,
        data['${effectivePrefix}synced'],
      )!,
      dependsOnAssetId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}depends_on_asset_id'],
      ),
      createdAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}created_at'],
      )!,
    );
  }

  @override
  $OplogTable createAlias(String alias) {
    return $OplogTable(attachedDatabase, alias);
  }
}

class OplogRow extends DataClass implements Insertable<OplogRow> {
  final int seq;
  final String entityTable;
  final String entityId;
  final String op;
  final String hlcPacked;
  final String payloadHash;
  final bool synced;
  final String? dependsOnAssetId;
  final int createdAt;
  const OplogRow({
    required this.seq,
    required this.entityTable,
    required this.entityId,
    required this.op,
    required this.hlcPacked,
    required this.payloadHash,
    required this.synced,
    this.dependsOnAssetId,
    required this.createdAt,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['seq'] = Variable<int>(seq);
    map['entity_table'] = Variable<String>(entityTable);
    map['entity_id'] = Variable<String>(entityId);
    map['op'] = Variable<String>(op);
    map['hlc_packed'] = Variable<String>(hlcPacked);
    map['payload_hash'] = Variable<String>(payloadHash);
    map['synced'] = Variable<bool>(synced);
    if (!nullToAbsent || dependsOnAssetId != null) {
      map['depends_on_asset_id'] = Variable<String>(dependsOnAssetId);
    }
    map['created_at'] = Variable<int>(createdAt);
    return map;
  }

  OplogCompanion toCompanion(bool nullToAbsent) {
    return OplogCompanion(
      seq: Value(seq),
      entityTable: Value(entityTable),
      entityId: Value(entityId),
      op: Value(op),
      hlcPacked: Value(hlcPacked),
      payloadHash: Value(payloadHash),
      synced: Value(synced),
      dependsOnAssetId: dependsOnAssetId == null && nullToAbsent
          ? const Value.absent()
          : Value(dependsOnAssetId),
      createdAt: Value(createdAt),
    );
  }

  factory OplogRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return OplogRow(
      seq: serializer.fromJson<int>(json['seq']),
      entityTable: serializer.fromJson<String>(json['entityTable']),
      entityId: serializer.fromJson<String>(json['entityId']),
      op: serializer.fromJson<String>(json['op']),
      hlcPacked: serializer.fromJson<String>(json['hlcPacked']),
      payloadHash: serializer.fromJson<String>(json['payloadHash']),
      synced: serializer.fromJson<bool>(json['synced']),
      dependsOnAssetId: serializer.fromJson<String?>(json['dependsOnAssetId']),
      createdAt: serializer.fromJson<int>(json['createdAt']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'seq': serializer.toJson<int>(seq),
      'entityTable': serializer.toJson<String>(entityTable),
      'entityId': serializer.toJson<String>(entityId),
      'op': serializer.toJson<String>(op),
      'hlcPacked': serializer.toJson<String>(hlcPacked),
      'payloadHash': serializer.toJson<String>(payloadHash),
      'synced': serializer.toJson<bool>(synced),
      'dependsOnAssetId': serializer.toJson<String?>(dependsOnAssetId),
      'createdAt': serializer.toJson<int>(createdAt),
    };
  }

  OplogRow copyWith({
    int? seq,
    String? entityTable,
    String? entityId,
    String? op,
    String? hlcPacked,
    String? payloadHash,
    bool? synced,
    Value<String?> dependsOnAssetId = const Value.absent(),
    int? createdAt,
  }) => OplogRow(
    seq: seq ?? this.seq,
    entityTable: entityTable ?? this.entityTable,
    entityId: entityId ?? this.entityId,
    op: op ?? this.op,
    hlcPacked: hlcPacked ?? this.hlcPacked,
    payloadHash: payloadHash ?? this.payloadHash,
    synced: synced ?? this.synced,
    dependsOnAssetId: dependsOnAssetId.present
        ? dependsOnAssetId.value
        : this.dependsOnAssetId,
    createdAt: createdAt ?? this.createdAt,
  );
  OplogRow copyWithCompanion(OplogCompanion data) {
    return OplogRow(
      seq: data.seq.present ? data.seq.value : this.seq,
      entityTable: data.entityTable.present
          ? data.entityTable.value
          : this.entityTable,
      entityId: data.entityId.present ? data.entityId.value : this.entityId,
      op: data.op.present ? data.op.value : this.op,
      hlcPacked: data.hlcPacked.present ? data.hlcPacked.value : this.hlcPacked,
      payloadHash: data.payloadHash.present
          ? data.payloadHash.value
          : this.payloadHash,
      synced: data.synced.present ? data.synced.value : this.synced,
      dependsOnAssetId: data.dependsOnAssetId.present
          ? data.dependsOnAssetId.value
          : this.dependsOnAssetId,
      createdAt: data.createdAt.present ? data.createdAt.value : this.createdAt,
    );
  }

  @override
  String toString() {
    return (StringBuffer('OplogRow(')
          ..write('seq: $seq, ')
          ..write('entityTable: $entityTable, ')
          ..write('entityId: $entityId, ')
          ..write('op: $op, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('payloadHash: $payloadHash, ')
          ..write('synced: $synced, ')
          ..write('dependsOnAssetId: $dependsOnAssetId, ')
          ..write('createdAt: $createdAt')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(
    seq,
    entityTable,
    entityId,
    op,
    hlcPacked,
    payloadHash,
    synced,
    dependsOnAssetId,
    createdAt,
  );
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is OplogRow &&
          other.seq == this.seq &&
          other.entityTable == this.entityTable &&
          other.entityId == this.entityId &&
          other.op == this.op &&
          other.hlcPacked == this.hlcPacked &&
          other.payloadHash == this.payloadHash &&
          other.synced == this.synced &&
          other.dependsOnAssetId == this.dependsOnAssetId &&
          other.createdAt == this.createdAt);
}

class OplogCompanion extends UpdateCompanion<OplogRow> {
  final Value<int> seq;
  final Value<String> entityTable;
  final Value<String> entityId;
  final Value<String> op;
  final Value<String> hlcPacked;
  final Value<String> payloadHash;
  final Value<bool> synced;
  final Value<String?> dependsOnAssetId;
  final Value<int> createdAt;
  const OplogCompanion({
    this.seq = const Value.absent(),
    this.entityTable = const Value.absent(),
    this.entityId = const Value.absent(),
    this.op = const Value.absent(),
    this.hlcPacked = const Value.absent(),
    this.payloadHash = const Value.absent(),
    this.synced = const Value.absent(),
    this.dependsOnAssetId = const Value.absent(),
    this.createdAt = const Value.absent(),
  });
  OplogCompanion.insert({
    this.seq = const Value.absent(),
    required String entityTable,
    required String entityId,
    required String op,
    required String hlcPacked,
    required String payloadHash,
    this.synced = const Value.absent(),
    this.dependsOnAssetId = const Value.absent(),
    required int createdAt,
  }) : entityTable = Value(entityTable),
       entityId = Value(entityId),
       op = Value(op),
       hlcPacked = Value(hlcPacked),
       payloadHash = Value(payloadHash),
       createdAt = Value(createdAt);
  static Insertable<OplogRow> custom({
    Expression<int>? seq,
    Expression<String>? entityTable,
    Expression<String>? entityId,
    Expression<String>? op,
    Expression<String>? hlcPacked,
    Expression<String>? payloadHash,
    Expression<bool>? synced,
    Expression<String>? dependsOnAssetId,
    Expression<int>? createdAt,
  }) {
    return RawValuesInsertable({
      if (seq != null) 'seq': seq,
      if (entityTable != null) 'entity_table': entityTable,
      if (entityId != null) 'entity_id': entityId,
      if (op != null) 'op': op,
      if (hlcPacked != null) 'hlc_packed': hlcPacked,
      if (payloadHash != null) 'payload_hash': payloadHash,
      if (synced != null) 'synced': synced,
      if (dependsOnAssetId != null) 'depends_on_asset_id': dependsOnAssetId,
      if (createdAt != null) 'created_at': createdAt,
    });
  }

  OplogCompanion copyWith({
    Value<int>? seq,
    Value<String>? entityTable,
    Value<String>? entityId,
    Value<String>? op,
    Value<String>? hlcPacked,
    Value<String>? payloadHash,
    Value<bool>? synced,
    Value<String?>? dependsOnAssetId,
    Value<int>? createdAt,
  }) {
    return OplogCompanion(
      seq: seq ?? this.seq,
      entityTable: entityTable ?? this.entityTable,
      entityId: entityId ?? this.entityId,
      op: op ?? this.op,
      hlcPacked: hlcPacked ?? this.hlcPacked,
      payloadHash: payloadHash ?? this.payloadHash,
      synced: synced ?? this.synced,
      dependsOnAssetId: dependsOnAssetId ?? this.dependsOnAssetId,
      createdAt: createdAt ?? this.createdAt,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (seq.present) {
      map['seq'] = Variable<int>(seq.value);
    }
    if (entityTable.present) {
      map['entity_table'] = Variable<String>(entityTable.value);
    }
    if (entityId.present) {
      map['entity_id'] = Variable<String>(entityId.value);
    }
    if (op.present) {
      map['op'] = Variable<String>(op.value);
    }
    if (hlcPacked.present) {
      map['hlc_packed'] = Variable<String>(hlcPacked.value);
    }
    if (payloadHash.present) {
      map['payload_hash'] = Variable<String>(payloadHash.value);
    }
    if (synced.present) {
      map['synced'] = Variable<bool>(synced.value);
    }
    if (dependsOnAssetId.present) {
      map['depends_on_asset_id'] = Variable<String>(dependsOnAssetId.value);
    }
    if (createdAt.present) {
      map['created_at'] = Variable<int>(createdAt.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('OplogCompanion(')
          ..write('seq: $seq, ')
          ..write('entityTable: $entityTable, ')
          ..write('entityId: $entityId, ')
          ..write('op: $op, ')
          ..write('hlcPacked: $hlcPacked, ')
          ..write('payloadHash: $payloadHash, ')
          ..write('synced: $synced, ')
          ..write('dependsOnAssetId: $dependsOnAssetId, ')
          ..write('createdAt: $createdAt')
          ..write(')'))
        .toString();
  }
}

class $RecordingsInflightTable extends RecordingsInflight
    with TableInfo<$RecordingsInflightTable, RecordingInflightRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $RecordingsInflightTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _idMeta = const VerificationMeta('id');
  @override
  late final GeneratedColumn<String> id = GeneratedColumn<String>(
    'id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _practiceIdMeta = const VerificationMeta(
    'practiceId',
  );
  @override
  late final GeneratedColumn<String> practiceId = GeneratedColumn<String>(
    'practice_id',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _pidMeta = const VerificationMeta('pid');
  @override
  late final GeneratedColumn<int> pid = GeneratedColumn<int>(
    'pid',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _startedAtMeta = const VerificationMeta(
    'startedAt',
  );
  @override
  late final GeneratedColumn<int> startedAt = GeneratedColumn<int>(
    'started_at',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  @override
  List<GeneratedColumn> get $columns => [id, practiceId, pid, startedAt];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'recordings_inflight';
  @override
  VerificationContext validateIntegrity(
    Insertable<RecordingInflightRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('id')) {
      context.handle(_idMeta, id.isAcceptableOrUnknown(data['id']!, _idMeta));
    } else if (isInserting) {
      context.missing(_idMeta);
    }
    if (data.containsKey('practice_id')) {
      context.handle(
        _practiceIdMeta,
        practiceId.isAcceptableOrUnknown(data['practice_id']!, _practiceIdMeta),
      );
    } else if (isInserting) {
      context.missing(_practiceIdMeta);
    }
    if (data.containsKey('pid')) {
      context.handle(
        _pidMeta,
        pid.isAcceptableOrUnknown(data['pid']!, _pidMeta),
      );
    } else if (isInserting) {
      context.missing(_pidMeta);
    }
    if (data.containsKey('started_at')) {
      context.handle(
        _startedAtMeta,
        startedAt.isAcceptableOrUnknown(data['started_at']!, _startedAtMeta),
      );
    } else if (isInserting) {
      context.missing(_startedAtMeta);
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {id};
  @override
  RecordingInflightRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return RecordingInflightRow(
      id: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}id'],
      )!,
      practiceId: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}practice_id'],
      )!,
      pid: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}pid'],
      )!,
      startedAt: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}started_at'],
      )!,
    );
  }

  @override
  $RecordingsInflightTable createAlias(String alias) {
    return $RecordingsInflightTable(attachedDatabase, alias);
  }
}

class RecordingInflightRow extends DataClass
    implements Insertable<RecordingInflightRow> {
  final String id;
  final String practiceId;
  final int pid;
  final int startedAt;
  const RecordingInflightRow({
    required this.id,
    required this.practiceId,
    required this.pid,
    required this.startedAt,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['id'] = Variable<String>(id);
    map['practice_id'] = Variable<String>(practiceId);
    map['pid'] = Variable<int>(pid);
    map['started_at'] = Variable<int>(startedAt);
    return map;
  }

  RecordingsInflightCompanion toCompanion(bool nullToAbsent) {
    return RecordingsInflightCompanion(
      id: Value(id),
      practiceId: Value(practiceId),
      pid: Value(pid),
      startedAt: Value(startedAt),
    );
  }

  factory RecordingInflightRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return RecordingInflightRow(
      id: serializer.fromJson<String>(json['id']),
      practiceId: serializer.fromJson<String>(json['practiceId']),
      pid: serializer.fromJson<int>(json['pid']),
      startedAt: serializer.fromJson<int>(json['startedAt']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'id': serializer.toJson<String>(id),
      'practiceId': serializer.toJson<String>(practiceId),
      'pid': serializer.toJson<int>(pid),
      'startedAt': serializer.toJson<int>(startedAt),
    };
  }

  RecordingInflightRow copyWith({
    String? id,
    String? practiceId,
    int? pid,
    int? startedAt,
  }) => RecordingInflightRow(
    id: id ?? this.id,
    practiceId: practiceId ?? this.practiceId,
    pid: pid ?? this.pid,
    startedAt: startedAt ?? this.startedAt,
  );
  RecordingInflightRow copyWithCompanion(RecordingsInflightCompanion data) {
    return RecordingInflightRow(
      id: data.id.present ? data.id.value : this.id,
      practiceId: data.practiceId.present
          ? data.practiceId.value
          : this.practiceId,
      pid: data.pid.present ? data.pid.value : this.pid,
      startedAt: data.startedAt.present ? data.startedAt.value : this.startedAt,
    );
  }

  @override
  String toString() {
    return (StringBuffer('RecordingInflightRow(')
          ..write('id: $id, ')
          ..write('practiceId: $practiceId, ')
          ..write('pid: $pid, ')
          ..write('startedAt: $startedAt')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode => Object.hash(id, practiceId, pid, startedAt);
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is RecordingInflightRow &&
          other.id == this.id &&
          other.practiceId == this.practiceId &&
          other.pid == this.pid &&
          other.startedAt == this.startedAt);
}

class RecordingsInflightCompanion
    extends UpdateCompanion<RecordingInflightRow> {
  final Value<String> id;
  final Value<String> practiceId;
  final Value<int> pid;
  final Value<int> startedAt;
  final Value<int> rowid;
  const RecordingsInflightCompanion({
    this.id = const Value.absent(),
    this.practiceId = const Value.absent(),
    this.pid = const Value.absent(),
    this.startedAt = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  RecordingsInflightCompanion.insert({
    required String id,
    required String practiceId,
    required int pid,
    required int startedAt,
    this.rowid = const Value.absent(),
  }) : id = Value(id),
       practiceId = Value(practiceId),
       pid = Value(pid),
       startedAt = Value(startedAt);
  static Insertable<RecordingInflightRow> custom({
    Expression<String>? id,
    Expression<String>? practiceId,
    Expression<int>? pid,
    Expression<int>? startedAt,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (id != null) 'id': id,
      if (practiceId != null) 'practice_id': practiceId,
      if (pid != null) 'pid': pid,
      if (startedAt != null) 'started_at': startedAt,
      if (rowid != null) 'rowid': rowid,
    });
  }

  RecordingsInflightCompanion copyWith({
    Value<String>? id,
    Value<String>? practiceId,
    Value<int>? pid,
    Value<int>? startedAt,
    Value<int>? rowid,
  }) {
    return RecordingsInflightCompanion(
      id: id ?? this.id,
      practiceId: practiceId ?? this.practiceId,
      pid: pid ?? this.pid,
      startedAt: startedAt ?? this.startedAt,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (id.present) {
      map['id'] = Variable<String>(id.value);
    }
    if (practiceId.present) {
      map['practice_id'] = Variable<String>(practiceId.value);
    }
    if (pid.present) {
      map['pid'] = Variable<int>(pid.value);
    }
    if (startedAt.present) {
      map['started_at'] = Variable<int>(startedAt.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('RecordingsInflightCompanion(')
          ..write('id: $id, ')
          ..write('practiceId: $practiceId, ')
          ..write('pid: $pid, ')
          ..write('startedAt: $startedAt, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

class $WaveformCacheTable extends WaveformCache
    with TableInfo<$WaveformCacheTable, WaveformCacheRow> {
  @override
  final GeneratedDatabase attachedDatabase;
  final String? _alias;
  $WaveformCacheTable(this.attachedDatabase, [this._alias]);
  static const VerificationMeta _contentHashMeta = const VerificationMeta(
    'contentHash',
  );
  @override
  late final GeneratedColumn<String> contentHash = GeneratedColumn<String>(
    'content_hash',
    aliasedName,
    false,
    type: DriftSqlType.string,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _algoVersionMeta = const VerificationMeta(
    'algoVersion',
  );
  @override
  late final GeneratedColumn<int> algoVersion = GeneratedColumn<int>(
    'algo_version',
    aliasedName,
    false,
    type: DriftSqlType.int,
    requiredDuringInsert: true,
  );
  static const VerificationMeta _peaksMeta = const VerificationMeta('peaks');
  @override
  late final GeneratedColumn<Uint8List> peaks = GeneratedColumn<Uint8List>(
    'peaks',
    aliasedName,
    false,
    type: DriftSqlType.blob,
    requiredDuringInsert: true,
  );
  @override
  List<GeneratedColumn> get $columns => [contentHash, algoVersion, peaks];
  @override
  String get aliasedName => _alias ?? actualTableName;
  @override
  String get actualTableName => $name;
  static const String $name = 'waveform_cache';
  @override
  VerificationContext validateIntegrity(
    Insertable<WaveformCacheRow> instance, {
    bool isInserting = false,
  }) {
    final context = VerificationContext();
    final data = instance.toColumns(true);
    if (data.containsKey('content_hash')) {
      context.handle(
        _contentHashMeta,
        contentHash.isAcceptableOrUnknown(
          data['content_hash']!,
          _contentHashMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_contentHashMeta);
    }
    if (data.containsKey('algo_version')) {
      context.handle(
        _algoVersionMeta,
        algoVersion.isAcceptableOrUnknown(
          data['algo_version']!,
          _algoVersionMeta,
        ),
      );
    } else if (isInserting) {
      context.missing(_algoVersionMeta);
    }
    if (data.containsKey('peaks')) {
      context.handle(
        _peaksMeta,
        peaks.isAcceptableOrUnknown(data['peaks']!, _peaksMeta),
      );
    } else if (isInserting) {
      context.missing(_peaksMeta);
    }
    return context;
  }

  @override
  Set<GeneratedColumn> get $primaryKey => {contentHash, algoVersion};
  @override
  WaveformCacheRow map(Map<String, dynamic> data, {String? tablePrefix}) {
    final effectivePrefix = tablePrefix != null ? '$tablePrefix.' : '';
    return WaveformCacheRow(
      contentHash: attachedDatabase.typeMapping.read(
        DriftSqlType.string,
        data['${effectivePrefix}content_hash'],
      )!,
      algoVersion: attachedDatabase.typeMapping.read(
        DriftSqlType.int,
        data['${effectivePrefix}algo_version'],
      )!,
      peaks: attachedDatabase.typeMapping.read(
        DriftSqlType.blob,
        data['${effectivePrefix}peaks'],
      )!,
    );
  }

  @override
  $WaveformCacheTable createAlias(String alias) {
    return $WaveformCacheTable(attachedDatabase, alias);
  }
}

class WaveformCacheRow extends DataClass
    implements Insertable<WaveformCacheRow> {
  final String contentHash;
  final int algoVersion;
  final Uint8List peaks;
  const WaveformCacheRow({
    required this.contentHash,
    required this.algoVersion,
    required this.peaks,
  });
  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    map['content_hash'] = Variable<String>(contentHash);
    map['algo_version'] = Variable<int>(algoVersion);
    map['peaks'] = Variable<Uint8List>(peaks);
    return map;
  }

  WaveformCacheCompanion toCompanion(bool nullToAbsent) {
    return WaveformCacheCompanion(
      contentHash: Value(contentHash),
      algoVersion: Value(algoVersion),
      peaks: Value(peaks),
    );
  }

  factory WaveformCacheRow.fromJson(
    Map<String, dynamic> json, {
    ValueSerializer? serializer,
  }) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return WaveformCacheRow(
      contentHash: serializer.fromJson<String>(json['contentHash']),
      algoVersion: serializer.fromJson<int>(json['algoVersion']),
      peaks: serializer.fromJson<Uint8List>(json['peaks']),
    );
  }
  @override
  Map<String, dynamic> toJson({ValueSerializer? serializer}) {
    serializer ??= driftRuntimeOptions.defaultSerializer;
    return <String, dynamic>{
      'contentHash': serializer.toJson<String>(contentHash),
      'algoVersion': serializer.toJson<int>(algoVersion),
      'peaks': serializer.toJson<Uint8List>(peaks),
    };
  }

  WaveformCacheRow copyWith({
    String? contentHash,
    int? algoVersion,
    Uint8List? peaks,
  }) => WaveformCacheRow(
    contentHash: contentHash ?? this.contentHash,
    algoVersion: algoVersion ?? this.algoVersion,
    peaks: peaks ?? this.peaks,
  );
  WaveformCacheRow copyWithCompanion(WaveformCacheCompanion data) {
    return WaveformCacheRow(
      contentHash: data.contentHash.present
          ? data.contentHash.value
          : this.contentHash,
      algoVersion: data.algoVersion.present
          ? data.algoVersion.value
          : this.algoVersion,
      peaks: data.peaks.present ? data.peaks.value : this.peaks,
    );
  }

  @override
  String toString() {
    return (StringBuffer('WaveformCacheRow(')
          ..write('contentHash: $contentHash, ')
          ..write('algoVersion: $algoVersion, ')
          ..write('peaks: $peaks')
          ..write(')'))
        .toString();
  }

  @override
  int get hashCode =>
      Object.hash(contentHash, algoVersion, $driftBlobEquality.hash(peaks));
  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      (other is WaveformCacheRow &&
          other.contentHash == this.contentHash &&
          other.algoVersion == this.algoVersion &&
          $driftBlobEquality.equals(other.peaks, this.peaks));
}

class WaveformCacheCompanion extends UpdateCompanion<WaveformCacheRow> {
  final Value<String> contentHash;
  final Value<int> algoVersion;
  final Value<Uint8List> peaks;
  final Value<int> rowid;
  const WaveformCacheCompanion({
    this.contentHash = const Value.absent(),
    this.algoVersion = const Value.absent(),
    this.peaks = const Value.absent(),
    this.rowid = const Value.absent(),
  });
  WaveformCacheCompanion.insert({
    required String contentHash,
    required int algoVersion,
    required Uint8List peaks,
    this.rowid = const Value.absent(),
  }) : contentHash = Value(contentHash),
       algoVersion = Value(algoVersion),
       peaks = Value(peaks);
  static Insertable<WaveformCacheRow> custom({
    Expression<String>? contentHash,
    Expression<int>? algoVersion,
    Expression<Uint8List>? peaks,
    Expression<int>? rowid,
  }) {
    return RawValuesInsertable({
      if (contentHash != null) 'content_hash': contentHash,
      if (algoVersion != null) 'algo_version': algoVersion,
      if (peaks != null) 'peaks': peaks,
      if (rowid != null) 'rowid': rowid,
    });
  }

  WaveformCacheCompanion copyWith({
    Value<String>? contentHash,
    Value<int>? algoVersion,
    Value<Uint8List>? peaks,
    Value<int>? rowid,
  }) {
    return WaveformCacheCompanion(
      contentHash: contentHash ?? this.contentHash,
      algoVersion: algoVersion ?? this.algoVersion,
      peaks: peaks ?? this.peaks,
      rowid: rowid ?? this.rowid,
    );
  }

  @override
  Map<String, Expression> toColumns(bool nullToAbsent) {
    final map = <String, Expression>{};
    if (contentHash.present) {
      map['content_hash'] = Variable<String>(contentHash.value);
    }
    if (algoVersion.present) {
      map['algo_version'] = Variable<int>(algoVersion.value);
    }
    if (peaks.present) {
      map['peaks'] = Variable<Uint8List>(peaks.value);
    }
    if (rowid.present) {
      map['rowid'] = Variable<int>(rowid.value);
    }
    return map;
  }

  @override
  String toString() {
    return (StringBuffer('WaveformCacheCompanion(')
          ..write('contentHash: $contentHash, ')
          ..write('algoVersion: $algoVersion, ')
          ..write('peaks: $peaks, ')
          ..write('rowid: $rowid')
          ..write(')'))
        .toString();
  }
}

abstract class _$AppDatabase extends GeneratedDatabase {
  _$AppDatabase(QueryExecutor e) : super(e);
  $AppDatabaseManager get managers => $AppDatabaseManager(this);
  late final $AssetsTable assets = $AssetsTable(this);
  late final $PracticesTable practices = $PracticesTable(this);
  late final $InterpretationsTable interpretations = $InterpretationsTable(
    this,
  );
  late final $FxBusesTable fxBuses = $FxBusesTable(this);
  late final $TracksTable tracks = $TracksTable(this);
  late final $ClipsTable clips = $ClipsTable(this);
  late final $PracticeNotesTable practiceNotes = $PracticeNotesTable(this);
  late final $TechniqueMarkersTable techniqueMarkers = $TechniqueMarkersTable(
    this,
  );
  late final $TechniquesTable techniques = $TechniquesTable(this);
  late final $TechniqueMediaTable techniqueMedia = $TechniqueMediaTable(this);
  late final $OplogTable oplog = $OplogTable(this);
  late final $RecordingsInflightTable recordingsInflight =
      $RecordingsInflightTable(this);
  late final $WaveformCacheTable waveformCache = $WaveformCacheTable(this);
  @override
  Iterable<TableInfo<Table, Object?>> get allTables =>
      allSchemaEntities.whereType<TableInfo<Table, Object?>>();
  @override
  List<DatabaseSchemaEntity> get allSchemaEntities => [
    assets,
    practices,
    interpretations,
    fxBuses,
    tracks,
    clips,
    practiceNotes,
    techniqueMarkers,
    techniques,
    techniqueMedia,
    oplog,
    recordingsInflight,
    waveformCache,
  ];
}

typedef $$AssetsTableCreateCompanionBuilder =
    AssetsCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String contentHash,
      required int byteSize,
      Value<String> ext,
      Value<String> kind,
      Value<String?> localRelPath,
      Value<String> recordSyncState,
      Value<String> bytesSyncState,
      Value<String?> remoteRef,
      Value<int> rowid,
    });
typedef $$AssetsTableUpdateCompanionBuilder =
    AssetsCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> contentHash,
      Value<int> byteSize,
      Value<String> ext,
      Value<String> kind,
      Value<String?> localRelPath,
      Value<String> recordSyncState,
      Value<String> bytesSyncState,
      Value<String?> remoteRef,
      Value<int> rowid,
    });

final class $$AssetsTableReferences
    extends BaseReferences<_$AppDatabase, $AssetsTable, AssetRow> {
  $$AssetsTableReferences(super.$_db, super.$_table, super.$_typedResult);

  static MultiTypedResultKey<$PracticesTable, List<PracticeRow>>
  _practicesRefsTable(_$AppDatabase db) => MultiTypedResultKey.fromTable(
    db.practices,
    aliasName: 'assets__id__practices__separation_asset_id',
  );

  $$PracticesTableProcessedTableManager get practicesRefs {
    final manager = $$PracticesTableTableManager($_db, $_db.practices).filter(
      (f) => f.separationAssetId.id.sqlEquals($_itemColumn<String>('id')!),
    );

    final cache = $_typedResult.readTableOrNull(_practicesRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }

  static MultiTypedResultKey<$ClipsTable, List<ClipRow>> _clipsRefsTable(
    _$AppDatabase db,
  ) => MultiTypedResultKey.fromTable(
    db.clips,
    aliasName: 'assets__id__clips__asset_id',
  );

  $$ClipsTableProcessedTableManager get clipsRefs {
    final manager = $$ClipsTableTableManager(
      $_db,
      $_db.clips,
    ).filter((f) => f.assetId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_clipsRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }

  static MultiTypedResultKey<$TechniqueMediaTable, List<TechniqueMediaRow>>
  _techniqueMediaRefsTable(_$AppDatabase db) => MultiTypedResultKey.fromTable(
    db.techniqueMedia,
    aliasName: 'assets__id__technique_media__asset_id',
  );

  $$TechniqueMediaTableProcessedTableManager get techniqueMediaRefs {
    final manager = $$TechniqueMediaTableTableManager(
      $_db,
      $_db.techniqueMedia,
    ).filter((f) => f.assetId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_techniqueMediaRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }
}

class $$AssetsTableFilterComposer
    extends Composer<_$AppDatabase, $AssetsTable> {
  $$AssetsTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get contentHash => $composableBuilder(
    column: $table.contentHash,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get byteSize => $composableBuilder(
    column: $table.byteSize,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get ext => $composableBuilder(
    column: $table.ext,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get kind => $composableBuilder(
    column: $table.kind,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get localRelPath => $composableBuilder(
    column: $table.localRelPath,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get recordSyncState => $composableBuilder(
    column: $table.recordSyncState,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get bytesSyncState => $composableBuilder(
    column: $table.bytesSyncState,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get remoteRef => $composableBuilder(
    column: $table.remoteRef,
    builder: (column) => ColumnFilters(column),
  );

  Expression<bool> practicesRefs(
    Expression<bool> Function($$PracticesTableFilterComposer f) f,
  ) {
    final $$PracticesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.separationAssetId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableFilterComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<bool> clipsRefs(
    Expression<bool> Function($$ClipsTableFilterComposer f) f,
  ) {
    final $$ClipsTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.clips,
      getReferencedColumn: (t) => t.assetId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$ClipsTableFilterComposer(
            $db: $db,
            $table: $db.clips,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<bool> techniqueMediaRefs(
    Expression<bool> Function($$TechniqueMediaTableFilterComposer f) f,
  ) {
    final $$TechniqueMediaTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.techniqueMedia,
      getReferencedColumn: (t) => t.assetId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniqueMediaTableFilterComposer(
            $db: $db,
            $table: $db.techniqueMedia,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$AssetsTableOrderingComposer
    extends Composer<_$AppDatabase, $AssetsTable> {
  $$AssetsTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get contentHash => $composableBuilder(
    column: $table.contentHash,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get byteSize => $composableBuilder(
    column: $table.byteSize,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get ext => $composableBuilder(
    column: $table.ext,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get kind => $composableBuilder(
    column: $table.kind,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get localRelPath => $composableBuilder(
    column: $table.localRelPath,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get recordSyncState => $composableBuilder(
    column: $table.recordSyncState,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get bytesSyncState => $composableBuilder(
    column: $table.bytesSyncState,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get remoteRef => $composableBuilder(
    column: $table.remoteRef,
    builder: (column) => ColumnOrderings(column),
  );
}

class $$AssetsTableAnnotationComposer
    extends Composer<_$AppDatabase, $AssetsTable> {
  $$AssetsTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<String> get contentHash => $composableBuilder(
    column: $table.contentHash,
    builder: (column) => column,
  );

  GeneratedColumn<int> get byteSize =>
      $composableBuilder(column: $table.byteSize, builder: (column) => column);

  GeneratedColumn<String> get ext =>
      $composableBuilder(column: $table.ext, builder: (column) => column);

  GeneratedColumn<String> get kind =>
      $composableBuilder(column: $table.kind, builder: (column) => column);

  GeneratedColumn<String> get localRelPath => $composableBuilder(
    column: $table.localRelPath,
    builder: (column) => column,
  );

  GeneratedColumn<String> get recordSyncState => $composableBuilder(
    column: $table.recordSyncState,
    builder: (column) => column,
  );

  GeneratedColumn<String> get bytesSyncState => $composableBuilder(
    column: $table.bytesSyncState,
    builder: (column) => column,
  );

  GeneratedColumn<String> get remoteRef =>
      $composableBuilder(column: $table.remoteRef, builder: (column) => column);

  Expression<T> practicesRefs<T extends Object>(
    Expression<T> Function($$PracticesTableAnnotationComposer a) f,
  ) {
    final $$PracticesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.separationAssetId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableAnnotationComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<T> clipsRefs<T extends Object>(
    Expression<T> Function($$ClipsTableAnnotationComposer a) f,
  ) {
    final $$ClipsTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.clips,
      getReferencedColumn: (t) => t.assetId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$ClipsTableAnnotationComposer(
            $db: $db,
            $table: $db.clips,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<T> techniqueMediaRefs<T extends Object>(
    Expression<T> Function($$TechniqueMediaTableAnnotationComposer a) f,
  ) {
    final $$TechniqueMediaTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.techniqueMedia,
      getReferencedColumn: (t) => t.assetId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniqueMediaTableAnnotationComposer(
            $db: $db,
            $table: $db.techniqueMedia,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$AssetsTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $AssetsTable,
          AssetRow,
          $$AssetsTableFilterComposer,
          $$AssetsTableOrderingComposer,
          $$AssetsTableAnnotationComposer,
          $$AssetsTableCreateCompanionBuilder,
          $$AssetsTableUpdateCompanionBuilder,
          (AssetRow, $$AssetsTableReferences),
          AssetRow,
          PrefetchHooks Function({
            bool practicesRefs,
            bool clipsRefs,
            bool techniqueMediaRefs,
          })
        > {
  $$AssetsTableTableManager(_$AppDatabase db, $AssetsTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$AssetsTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$AssetsTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$AssetsTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> contentHash = const Value.absent(),
                Value<int> byteSize = const Value.absent(),
                Value<String> ext = const Value.absent(),
                Value<String> kind = const Value.absent(),
                Value<String?> localRelPath = const Value.absent(),
                Value<String> recordSyncState = const Value.absent(),
                Value<String> bytesSyncState = const Value.absent(),
                Value<String?> remoteRef = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => AssetsCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                contentHash: contentHash,
                byteSize: byteSize,
                ext: ext,
                kind: kind,
                localRelPath: localRelPath,
                recordSyncState: recordSyncState,
                bytesSyncState: bytesSyncState,
                remoteRef: remoteRef,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String contentHash,
                required int byteSize,
                Value<String> ext = const Value.absent(),
                Value<String> kind = const Value.absent(),
                Value<String?> localRelPath = const Value.absent(),
                Value<String> recordSyncState = const Value.absent(),
                Value<String> bytesSyncState = const Value.absent(),
                Value<String?> remoteRef = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => AssetsCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                contentHash: contentHash,
                byteSize: byteSize,
                ext: ext,
                kind: kind,
                localRelPath: localRelPath,
                recordSyncState: recordSyncState,
                bytesSyncState: bytesSyncState,
                remoteRef: remoteRef,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) =>
                    (e.readTable(table), $$AssetsTableReferences(db, table, e)),
              )
              .toList(),
          prefetchHooksCallback:
              ({
                practicesRefs = false,
                clipsRefs = false,
                techniqueMediaRefs = false,
              }) {
                return PrefetchHooks(
                  db: db,
                  explicitlyWatchedTables: [
                    if (practicesRefs) db.practices,
                    if (clipsRefs) db.clips,
                    if (techniqueMediaRefs) db.techniqueMedia,
                  ],
                  addJoins: null,
                  getPrefetchedDataCallback: (items) async {
                    return [
                      if (practicesRefs)
                        await $_getPrefetchedData<
                          AssetRow,
                          $AssetsTable,
                          PracticeRow
                        >(
                          currentTable: table,
                          referencedTable: $$AssetsTableReferences
                              ._practicesRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$AssetsTableReferences(
                                db,
                                table,
                                p0,
                              ).practicesRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.separationAssetId == item.id,
                              ),
                          typedResults: items,
                        ),
                      if (clipsRefs)
                        await $_getPrefetchedData<
                          AssetRow,
                          $AssetsTable,
                          ClipRow
                        >(
                          currentTable: table,
                          referencedTable: $$AssetsTableReferences
                              ._clipsRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$AssetsTableReferences(db, table, p0).clipsRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.assetId == item.id,
                              ),
                          typedResults: items,
                        ),
                      if (techniqueMediaRefs)
                        await $_getPrefetchedData<
                          AssetRow,
                          $AssetsTable,
                          TechniqueMediaRow
                        >(
                          currentTable: table,
                          referencedTable: $$AssetsTableReferences
                              ._techniqueMediaRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$AssetsTableReferences(
                                db,
                                table,
                                p0,
                              ).techniqueMediaRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.assetId == item.id,
                              ),
                          typedResults: items,
                        ),
                    ];
                  },
                );
              },
        ),
      );
}

typedef $$AssetsTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $AssetsTable,
      AssetRow,
      $$AssetsTableFilterComposer,
      $$AssetsTableOrderingComposer,
      $$AssetsTableAnnotationComposer,
      $$AssetsTableCreateCompanionBuilder,
      $$AssetsTableUpdateCompanionBuilder,
      (AssetRow, $$AssetsTableReferences),
      AssetRow,
      PrefetchHooks Function({
        bool practicesRefs,
        bool clipsRefs,
        bool techniqueMediaRefs,
      })
    >;
typedef $$PracticesTableCreateCompanionBuilder =
    PracticesCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      Value<String> title,
      Value<int> timelineDurationMs,
      Value<double> masterVolume,
      Value<String?> activeInterpretationId,
      Value<String?> separationAssetId,
      Value<String?> reverbJson,
      Value<String?> lyricsJson,
      Value<int> rowid,
    });
typedef $$PracticesTableUpdateCompanionBuilder =
    PracticesCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> title,
      Value<int> timelineDurationMs,
      Value<double> masterVolume,
      Value<String?> activeInterpretationId,
      Value<String?> separationAssetId,
      Value<String?> reverbJson,
      Value<String?> lyricsJson,
      Value<int> rowid,
    });

final class $$PracticesTableReferences
    extends BaseReferences<_$AppDatabase, $PracticesTable, PracticeRow> {
  $$PracticesTableReferences(super.$_db, super.$_table, super.$_typedResult);

  static $AssetsTable _separationAssetIdTable(_$AppDatabase db) =>
      db.assets.createAlias('practices__separation_asset_id__assets__id');

  $$AssetsTableProcessedTableManager? get separationAssetId {
    final $_column = $_itemColumn<String>('separation_asset_id');
    if ($_column == null) return null;
    final manager = $$AssetsTableTableManager(
      $_db,
      $_db.assets,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_separationAssetIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static MultiTypedResultKey<$InterpretationsTable, List<InterpretationRow>>
  _interpretationsRefsTable(_$AppDatabase db) => MultiTypedResultKey.fromTable(
    db.interpretations,
    aliasName: 'practices__id__interpretations__practice_id',
  );

  $$InterpretationsTableProcessedTableManager get interpretationsRefs {
    final manager = $$InterpretationsTableTableManager(
      $_db,
      $_db.interpretations,
    ).filter((f) => f.practiceId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(
      _interpretationsRefsTable($_db),
    );
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }

  static MultiTypedResultKey<$FxBusesTable, List<FxBusRow>> _fxBusesRefsTable(
    _$AppDatabase db,
  ) => MultiTypedResultKey.fromTable(
    db.fxBuses,
    aliasName: 'practices__id__fx_buses__practice_id',
  );

  $$FxBusesTableProcessedTableManager get fxBusesRefs {
    final manager = $$FxBusesTableTableManager(
      $_db,
      $_db.fxBuses,
    ).filter((f) => f.practiceId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_fxBusesRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }

  static MultiTypedResultKey<$TracksTable, List<TrackRow>> _tracksRefsTable(
    _$AppDatabase db,
  ) => MultiTypedResultKey.fromTable(
    db.tracks,
    aliasName: 'practices__id__tracks__practice_id',
  );

  $$TracksTableProcessedTableManager get tracksRefs {
    final manager = $$TracksTableTableManager(
      $_db,
      $_db.tracks,
    ).filter((f) => f.practiceId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_tracksRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }
}

class $$PracticesTableFilterComposer
    extends Composer<_$AppDatabase, $PracticesTable> {
  $$PracticesTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get title => $composableBuilder(
    column: $table.title,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get timelineDurationMs => $composableBuilder(
    column: $table.timelineDurationMs,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<double> get masterVolume => $composableBuilder(
    column: $table.masterVolume,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get activeInterpretationId => $composableBuilder(
    column: $table.activeInterpretationId,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get reverbJson => $composableBuilder(
    column: $table.reverbJson,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get lyricsJson => $composableBuilder(
    column: $table.lyricsJson,
    builder: (column) => ColumnFilters(column),
  );

  $$AssetsTableFilterComposer get separationAssetId {
    final $$AssetsTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.separationAssetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableFilterComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<bool> interpretationsRefs(
    Expression<bool> Function($$InterpretationsTableFilterComposer f) f,
  ) {
    final $$InterpretationsTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.interpretations,
      getReferencedColumn: (t) => t.practiceId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$InterpretationsTableFilterComposer(
            $db: $db,
            $table: $db.interpretations,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<bool> fxBusesRefs(
    Expression<bool> Function($$FxBusesTableFilterComposer f) f,
  ) {
    final $$FxBusesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.fxBuses,
      getReferencedColumn: (t) => t.practiceId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$FxBusesTableFilterComposer(
            $db: $db,
            $table: $db.fxBuses,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<bool> tracksRefs(
    Expression<bool> Function($$TracksTableFilterComposer f) f,
  ) {
    final $$TracksTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.practiceId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableFilterComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$PracticesTableOrderingComposer
    extends Composer<_$AppDatabase, $PracticesTable> {
  $$PracticesTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get title => $composableBuilder(
    column: $table.title,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get timelineDurationMs => $composableBuilder(
    column: $table.timelineDurationMs,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<double> get masterVolume => $composableBuilder(
    column: $table.masterVolume,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get activeInterpretationId => $composableBuilder(
    column: $table.activeInterpretationId,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get reverbJson => $composableBuilder(
    column: $table.reverbJson,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get lyricsJson => $composableBuilder(
    column: $table.lyricsJson,
    builder: (column) => ColumnOrderings(column),
  );

  $$AssetsTableOrderingComposer get separationAssetId {
    final $$AssetsTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.separationAssetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableOrderingComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$PracticesTableAnnotationComposer
    extends Composer<_$AppDatabase, $PracticesTable> {
  $$PracticesTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<String> get title =>
      $composableBuilder(column: $table.title, builder: (column) => column);

  GeneratedColumn<int> get timelineDurationMs => $composableBuilder(
    column: $table.timelineDurationMs,
    builder: (column) => column,
  );

  GeneratedColumn<double> get masterVolume => $composableBuilder(
    column: $table.masterVolume,
    builder: (column) => column,
  );

  GeneratedColumn<String> get activeInterpretationId => $composableBuilder(
    column: $table.activeInterpretationId,
    builder: (column) => column,
  );

  GeneratedColumn<String> get reverbJson => $composableBuilder(
    column: $table.reverbJson,
    builder: (column) => column,
  );

  GeneratedColumn<String> get lyricsJson => $composableBuilder(
    column: $table.lyricsJson,
    builder: (column) => column,
  );

  $$AssetsTableAnnotationComposer get separationAssetId {
    final $$AssetsTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.separationAssetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableAnnotationComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<T> interpretationsRefs<T extends Object>(
    Expression<T> Function($$InterpretationsTableAnnotationComposer a) f,
  ) {
    final $$InterpretationsTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.interpretations,
      getReferencedColumn: (t) => t.practiceId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$InterpretationsTableAnnotationComposer(
            $db: $db,
            $table: $db.interpretations,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<T> fxBusesRefs<T extends Object>(
    Expression<T> Function($$FxBusesTableAnnotationComposer a) f,
  ) {
    final $$FxBusesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.fxBuses,
      getReferencedColumn: (t) => t.practiceId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$FxBusesTableAnnotationComposer(
            $db: $db,
            $table: $db.fxBuses,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<T> tracksRefs<T extends Object>(
    Expression<T> Function($$TracksTableAnnotationComposer a) f,
  ) {
    final $$TracksTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.practiceId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableAnnotationComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$PracticesTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $PracticesTable,
          PracticeRow,
          $$PracticesTableFilterComposer,
          $$PracticesTableOrderingComposer,
          $$PracticesTableAnnotationComposer,
          $$PracticesTableCreateCompanionBuilder,
          $$PracticesTableUpdateCompanionBuilder,
          (PracticeRow, $$PracticesTableReferences),
          PracticeRow,
          PrefetchHooks Function({
            bool separationAssetId,
            bool interpretationsRefs,
            bool fxBusesRefs,
            bool tracksRefs,
          })
        > {
  $$PracticesTableTableManager(_$AppDatabase db, $PracticesTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$PracticesTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$PracticesTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$PracticesTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> title = const Value.absent(),
                Value<int> timelineDurationMs = const Value.absent(),
                Value<double> masterVolume = const Value.absent(),
                Value<String?> activeInterpretationId = const Value.absent(),
                Value<String?> separationAssetId = const Value.absent(),
                Value<String?> reverbJson = const Value.absent(),
                Value<String?> lyricsJson = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => PracticesCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                title: title,
                timelineDurationMs: timelineDurationMs,
                masterVolume: masterVolume,
                activeInterpretationId: activeInterpretationId,
                separationAssetId: separationAssetId,
                reverbJson: reverbJson,
                lyricsJson: lyricsJson,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                Value<String> title = const Value.absent(),
                Value<int> timelineDurationMs = const Value.absent(),
                Value<double> masterVolume = const Value.absent(),
                Value<String?> activeInterpretationId = const Value.absent(),
                Value<String?> separationAssetId = const Value.absent(),
                Value<String?> reverbJson = const Value.absent(),
                Value<String?> lyricsJson = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => PracticesCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                title: title,
                timelineDurationMs: timelineDurationMs,
                masterVolume: masterVolume,
                activeInterpretationId: activeInterpretationId,
                separationAssetId: separationAssetId,
                reverbJson: reverbJson,
                lyricsJson: lyricsJson,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) => (
                  e.readTable(table),
                  $$PracticesTableReferences(db, table, e),
                ),
              )
              .toList(),
          prefetchHooksCallback:
              ({
                separationAssetId = false,
                interpretationsRefs = false,
                fxBusesRefs = false,
                tracksRefs = false,
              }) {
                return PrefetchHooks(
                  db: db,
                  explicitlyWatchedTables: [
                    if (interpretationsRefs) db.interpretations,
                    if (fxBusesRefs) db.fxBuses,
                    if (tracksRefs) db.tracks,
                  ],
                  addJoins:
                      <
                        T extends TableManagerState<
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic
                        >
                      >(state) {
                        if (separationAssetId) {
                          state =
                              state.withJoin(
                                    currentTable: table,
                                    currentColumn: table.separationAssetId,
                                    referencedTable: $$PracticesTableReferences
                                        ._separationAssetIdTable(db),
                                    referencedColumn: $$PracticesTableReferences
                                        ._separationAssetIdTable(db)
                                        .id,
                                  )
                                  as T;
                        }

                        return state;
                      },
                  getPrefetchedDataCallback: (items) async {
                    return [
                      if (interpretationsRefs)
                        await $_getPrefetchedData<
                          PracticeRow,
                          $PracticesTable,
                          InterpretationRow
                        >(
                          currentTable: table,
                          referencedTable: $$PracticesTableReferences
                              ._interpretationsRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$PracticesTableReferences(
                                db,
                                table,
                                p0,
                              ).interpretationsRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.practiceId == item.id,
                              ),
                          typedResults: items,
                        ),
                      if (fxBusesRefs)
                        await $_getPrefetchedData<
                          PracticeRow,
                          $PracticesTable,
                          FxBusRow
                        >(
                          currentTable: table,
                          referencedTable: $$PracticesTableReferences
                              ._fxBusesRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$PracticesTableReferences(
                                db,
                                table,
                                p0,
                              ).fxBusesRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.practiceId == item.id,
                              ),
                          typedResults: items,
                        ),
                      if (tracksRefs)
                        await $_getPrefetchedData<
                          PracticeRow,
                          $PracticesTable,
                          TrackRow
                        >(
                          currentTable: table,
                          referencedTable: $$PracticesTableReferences
                              ._tracksRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$PracticesTableReferences(
                                db,
                                table,
                                p0,
                              ).tracksRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.practiceId == item.id,
                              ),
                          typedResults: items,
                        ),
                    ];
                  },
                );
              },
        ),
      );
}

typedef $$PracticesTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $PracticesTable,
      PracticeRow,
      $$PracticesTableFilterComposer,
      $$PracticesTableOrderingComposer,
      $$PracticesTableAnnotationComposer,
      $$PracticesTableCreateCompanionBuilder,
      $$PracticesTableUpdateCompanionBuilder,
      (PracticeRow, $$PracticesTableReferences),
      PracticeRow,
      PrefetchHooks Function({
        bool separationAssetId,
        bool interpretationsRefs,
        bool fxBusesRefs,
        bool tracksRefs,
      })
    >;
typedef $$InterpretationsTableCreateCompanionBuilder =
    InterpretationsCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String practiceId,
      Value<String> name,
      Value<int> orderIndex,
      Value<int> rowid,
    });
typedef $$InterpretationsTableUpdateCompanionBuilder =
    InterpretationsCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> practiceId,
      Value<String> name,
      Value<int> orderIndex,
      Value<int> rowid,
    });

final class $$InterpretationsTableReferences
    extends
        BaseReferences<
          _$AppDatabase,
          $InterpretationsTable,
          InterpretationRow
        > {
  $$InterpretationsTableReferences(
    super.$_db,
    super.$_table,
    super.$_typedResult,
  );

  static $PracticesTable _practiceIdTable(_$AppDatabase db) =>
      db.practices.createAlias('interpretations__practice_id__practices__id');

  $$PracticesTableProcessedTableManager get practiceId {
    final $_column = $_itemColumn<String>('practice_id')!;

    final manager = $$PracticesTableTableManager(
      $_db,
      $_db.practices,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_practiceIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static MultiTypedResultKey<$TracksTable, List<TrackRow>> _tracksRefsTable(
    _$AppDatabase db,
  ) => MultiTypedResultKey.fromTable(
    db.tracks,
    aliasName: 'interpretations__id__tracks__interpretation_id',
  );

  $$TracksTableProcessedTableManager get tracksRefs {
    final manager = $$TracksTableTableManager($_db, $_db.tracks).filter(
      (f) => f.interpretationId.id.sqlEquals($_itemColumn<String>('id')!),
    );

    final cache = $_typedResult.readTableOrNull(_tracksRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }
}

class $$InterpretationsTableFilterComposer
    extends Composer<_$AppDatabase, $InterpretationsTable> {
  $$InterpretationsTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get name => $composableBuilder(
    column: $table.name,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnFilters(column),
  );

  $$PracticesTableFilterComposer get practiceId {
    final $$PracticesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableFilterComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<bool> tracksRefs(
    Expression<bool> Function($$TracksTableFilterComposer f) f,
  ) {
    final $$TracksTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.interpretationId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableFilterComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$InterpretationsTableOrderingComposer
    extends Composer<_$AppDatabase, $InterpretationsTable> {
  $$InterpretationsTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get name => $composableBuilder(
    column: $table.name,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnOrderings(column),
  );

  $$PracticesTableOrderingComposer get practiceId {
    final $$PracticesTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableOrderingComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$InterpretationsTableAnnotationComposer
    extends Composer<_$AppDatabase, $InterpretationsTable> {
  $$InterpretationsTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<String> get name =>
      $composableBuilder(column: $table.name, builder: (column) => column);

  GeneratedColumn<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => column,
  );

  $$PracticesTableAnnotationComposer get practiceId {
    final $$PracticesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableAnnotationComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<T> tracksRefs<T extends Object>(
    Expression<T> Function($$TracksTableAnnotationComposer a) f,
  ) {
    final $$TracksTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.interpretationId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableAnnotationComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$InterpretationsTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $InterpretationsTable,
          InterpretationRow,
          $$InterpretationsTableFilterComposer,
          $$InterpretationsTableOrderingComposer,
          $$InterpretationsTableAnnotationComposer,
          $$InterpretationsTableCreateCompanionBuilder,
          $$InterpretationsTableUpdateCompanionBuilder,
          (InterpretationRow, $$InterpretationsTableReferences),
          InterpretationRow,
          PrefetchHooks Function({bool practiceId, bool tracksRefs})
        > {
  $$InterpretationsTableTableManager(
    _$AppDatabase db,
    $InterpretationsTable table,
  ) : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$InterpretationsTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$InterpretationsTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$InterpretationsTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> practiceId = const Value.absent(),
                Value<String> name = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => InterpretationsCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                practiceId: practiceId,
                name: name,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String practiceId,
                Value<String> name = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => InterpretationsCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                practiceId: practiceId,
                name: name,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) => (
                  e.readTable(table),
                  $$InterpretationsTableReferences(db, table, e),
                ),
              )
              .toList(),
          prefetchHooksCallback: ({practiceId = false, tracksRefs = false}) {
            return PrefetchHooks(
              db: db,
              explicitlyWatchedTables: [if (tracksRefs) db.tracks],
              addJoins:
                  <
                    T extends TableManagerState<
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic
                    >
                  >(state) {
                    if (practiceId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.practiceId,
                                referencedTable:
                                    $$InterpretationsTableReferences
                                        ._practiceIdTable(db),
                                referencedColumn:
                                    $$InterpretationsTableReferences
                                        ._practiceIdTable(db)
                                        .id,
                              )
                              as T;
                    }

                    return state;
                  },
              getPrefetchedDataCallback: (items) async {
                return [
                  if (tracksRefs)
                    await $_getPrefetchedData<
                      InterpretationRow,
                      $InterpretationsTable,
                      TrackRow
                    >(
                      currentTable: table,
                      referencedTable: $$InterpretationsTableReferences
                          ._tracksRefsTable(db),
                      managerFromTypedResult: (p0) =>
                          $$InterpretationsTableReferences(
                            db,
                            table,
                            p0,
                          ).tracksRefs,
                      referencedItemsForCurrentItem: (item, referencedItems) =>
                          referencedItems.where(
                            (e) => e.interpretationId == item.id,
                          ),
                      typedResults: items,
                    ),
                ];
              },
            );
          },
        ),
      );
}

typedef $$InterpretationsTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $InterpretationsTable,
      InterpretationRow,
      $$InterpretationsTableFilterComposer,
      $$InterpretationsTableOrderingComposer,
      $$InterpretationsTableAnnotationComposer,
      $$InterpretationsTableCreateCompanionBuilder,
      $$InterpretationsTableUpdateCompanionBuilder,
      (InterpretationRow, $$InterpretationsTableReferences),
      InterpretationRow,
      PrefetchHooks Function({bool practiceId, bool tracksRefs})
    >;
typedef $$FxBusesTableCreateCompanionBuilder =
    FxBusesCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String practiceId,
      Value<String> name,
      Value<int?> colorValue,
      Value<double> volume,
      Value<double> pan,
      Value<bool> muted,
      Value<bool> soloed,
      Value<String?> outputBusId,
      Value<String?> fxJson,
      Value<String?> sendsJson,
      Value<int> orderIndex,
      Value<int> rowid,
    });
typedef $$FxBusesTableUpdateCompanionBuilder =
    FxBusesCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> practiceId,
      Value<String> name,
      Value<int?> colorValue,
      Value<double> volume,
      Value<double> pan,
      Value<bool> muted,
      Value<bool> soloed,
      Value<String?> outputBusId,
      Value<String?> fxJson,
      Value<String?> sendsJson,
      Value<int> orderIndex,
      Value<int> rowid,
    });

final class $$FxBusesTableReferences
    extends BaseReferences<_$AppDatabase, $FxBusesTable, FxBusRow> {
  $$FxBusesTableReferences(super.$_db, super.$_table, super.$_typedResult);

  static $PracticesTable _practiceIdTable(_$AppDatabase db) =>
      db.practices.createAlias('fx_buses__practice_id__practices__id');

  $$PracticesTableProcessedTableManager get practiceId {
    final $_column = $_itemColumn<String>('practice_id')!;

    final manager = $$PracticesTableTableManager(
      $_db,
      $_db.practices,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_practiceIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static MultiTypedResultKey<$TracksTable, List<TrackRow>> _tracksRefsTable(
    _$AppDatabase db,
  ) => MultiTypedResultKey.fromTable(
    db.tracks,
    aliasName: 'fx_buses__id__tracks__bus_id',
  );

  $$TracksTableProcessedTableManager get tracksRefs {
    final manager = $$TracksTableTableManager(
      $_db,
      $_db.tracks,
    ).filter((f) => f.busId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_tracksRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }
}

class $$FxBusesTableFilterComposer
    extends Composer<_$AppDatabase, $FxBusesTable> {
  $$FxBusesTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get name => $composableBuilder(
    column: $table.name,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get colorValue => $composableBuilder(
    column: $table.colorValue,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<double> get volume => $composableBuilder(
    column: $table.volume,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<double> get pan => $composableBuilder(
    column: $table.pan,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<bool> get muted => $composableBuilder(
    column: $table.muted,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<bool> get soloed => $composableBuilder(
    column: $table.soloed,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get outputBusId => $composableBuilder(
    column: $table.outputBusId,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get fxJson => $composableBuilder(
    column: $table.fxJson,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get sendsJson => $composableBuilder(
    column: $table.sendsJson,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnFilters(column),
  );

  $$PracticesTableFilterComposer get practiceId {
    final $$PracticesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableFilterComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<bool> tracksRefs(
    Expression<bool> Function($$TracksTableFilterComposer f) f,
  ) {
    final $$TracksTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.busId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableFilterComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$FxBusesTableOrderingComposer
    extends Composer<_$AppDatabase, $FxBusesTable> {
  $$FxBusesTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get name => $composableBuilder(
    column: $table.name,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get colorValue => $composableBuilder(
    column: $table.colorValue,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<double> get volume => $composableBuilder(
    column: $table.volume,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<double> get pan => $composableBuilder(
    column: $table.pan,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<bool> get muted => $composableBuilder(
    column: $table.muted,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<bool> get soloed => $composableBuilder(
    column: $table.soloed,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get outputBusId => $composableBuilder(
    column: $table.outputBusId,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get fxJson => $composableBuilder(
    column: $table.fxJson,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get sendsJson => $composableBuilder(
    column: $table.sendsJson,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnOrderings(column),
  );

  $$PracticesTableOrderingComposer get practiceId {
    final $$PracticesTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableOrderingComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$FxBusesTableAnnotationComposer
    extends Composer<_$AppDatabase, $FxBusesTable> {
  $$FxBusesTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<String> get name =>
      $composableBuilder(column: $table.name, builder: (column) => column);

  GeneratedColumn<int> get colorValue => $composableBuilder(
    column: $table.colorValue,
    builder: (column) => column,
  );

  GeneratedColumn<double> get volume =>
      $composableBuilder(column: $table.volume, builder: (column) => column);

  GeneratedColumn<double> get pan =>
      $composableBuilder(column: $table.pan, builder: (column) => column);

  GeneratedColumn<bool> get muted =>
      $composableBuilder(column: $table.muted, builder: (column) => column);

  GeneratedColumn<bool> get soloed =>
      $composableBuilder(column: $table.soloed, builder: (column) => column);

  GeneratedColumn<String> get outputBusId => $composableBuilder(
    column: $table.outputBusId,
    builder: (column) => column,
  );

  GeneratedColumn<String> get fxJson =>
      $composableBuilder(column: $table.fxJson, builder: (column) => column);

  GeneratedColumn<String> get sendsJson =>
      $composableBuilder(column: $table.sendsJson, builder: (column) => column);

  GeneratedColumn<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => column,
  );

  $$PracticesTableAnnotationComposer get practiceId {
    final $$PracticesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableAnnotationComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<T> tracksRefs<T extends Object>(
    Expression<T> Function($$TracksTableAnnotationComposer a) f,
  ) {
    final $$TracksTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.busId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableAnnotationComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$FxBusesTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $FxBusesTable,
          FxBusRow,
          $$FxBusesTableFilterComposer,
          $$FxBusesTableOrderingComposer,
          $$FxBusesTableAnnotationComposer,
          $$FxBusesTableCreateCompanionBuilder,
          $$FxBusesTableUpdateCompanionBuilder,
          (FxBusRow, $$FxBusesTableReferences),
          FxBusRow,
          PrefetchHooks Function({bool practiceId, bool tracksRefs})
        > {
  $$FxBusesTableTableManager(_$AppDatabase db, $FxBusesTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$FxBusesTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$FxBusesTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$FxBusesTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> practiceId = const Value.absent(),
                Value<String> name = const Value.absent(),
                Value<int?> colorValue = const Value.absent(),
                Value<double> volume = const Value.absent(),
                Value<double> pan = const Value.absent(),
                Value<bool> muted = const Value.absent(),
                Value<bool> soloed = const Value.absent(),
                Value<String?> outputBusId = const Value.absent(),
                Value<String?> fxJson = const Value.absent(),
                Value<String?> sendsJson = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => FxBusesCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                practiceId: practiceId,
                name: name,
                colorValue: colorValue,
                volume: volume,
                pan: pan,
                muted: muted,
                soloed: soloed,
                outputBusId: outputBusId,
                fxJson: fxJson,
                sendsJson: sendsJson,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String practiceId,
                Value<String> name = const Value.absent(),
                Value<int?> colorValue = const Value.absent(),
                Value<double> volume = const Value.absent(),
                Value<double> pan = const Value.absent(),
                Value<bool> muted = const Value.absent(),
                Value<bool> soloed = const Value.absent(),
                Value<String?> outputBusId = const Value.absent(),
                Value<String?> fxJson = const Value.absent(),
                Value<String?> sendsJson = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => FxBusesCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                practiceId: practiceId,
                name: name,
                colorValue: colorValue,
                volume: volume,
                pan: pan,
                muted: muted,
                soloed: soloed,
                outputBusId: outputBusId,
                fxJson: fxJson,
                sendsJson: sendsJson,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) => (
                  e.readTable(table),
                  $$FxBusesTableReferences(db, table, e),
                ),
              )
              .toList(),
          prefetchHooksCallback: ({practiceId = false, tracksRefs = false}) {
            return PrefetchHooks(
              db: db,
              explicitlyWatchedTables: [if (tracksRefs) db.tracks],
              addJoins:
                  <
                    T extends TableManagerState<
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic
                    >
                  >(state) {
                    if (practiceId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.practiceId,
                                referencedTable: $$FxBusesTableReferences
                                    ._practiceIdTable(db),
                                referencedColumn: $$FxBusesTableReferences
                                    ._practiceIdTable(db)
                                    .id,
                              )
                              as T;
                    }

                    return state;
                  },
              getPrefetchedDataCallback: (items) async {
                return [
                  if (tracksRefs)
                    await $_getPrefetchedData<
                      FxBusRow,
                      $FxBusesTable,
                      TrackRow
                    >(
                      currentTable: table,
                      referencedTable: $$FxBusesTableReferences
                          ._tracksRefsTable(db),
                      managerFromTypedResult: (p0) =>
                          $$FxBusesTableReferences(db, table, p0).tracksRefs,
                      referencedItemsForCurrentItem: (item, referencedItems) =>
                          referencedItems.where((e) => e.busId == item.id),
                      typedResults: items,
                    ),
                ];
              },
            );
          },
        ),
      );
}

typedef $$FxBusesTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $FxBusesTable,
      FxBusRow,
      $$FxBusesTableFilterComposer,
      $$FxBusesTableOrderingComposer,
      $$FxBusesTableAnnotationComposer,
      $$FxBusesTableCreateCompanionBuilder,
      $$FxBusesTableUpdateCompanionBuilder,
      (FxBusRow, $$FxBusesTableReferences),
      FxBusRow,
      PrefetchHooks Function({bool practiceId, bool tracksRefs})
    >;
typedef $$TracksTableCreateCompanionBuilder =
    TracksCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String practiceId,
      required String kind,
      Value<String?> interpretationId,
      Value<String> displayName,
      Value<int?> colorValue,
      Value<bool> muted,
      Value<bool> soloed,
      Value<double> volume,
      Value<double> pan,
      Value<String?> busId,
      Value<String?> fxJson,
      Value<String?> sendsJson,
      Value<int> orderIndex,
      Value<int> rowid,
    });
typedef $$TracksTableUpdateCompanionBuilder =
    TracksCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> practiceId,
      Value<String> kind,
      Value<String?> interpretationId,
      Value<String> displayName,
      Value<int?> colorValue,
      Value<bool> muted,
      Value<bool> soloed,
      Value<double> volume,
      Value<double> pan,
      Value<String?> busId,
      Value<String?> fxJson,
      Value<String?> sendsJson,
      Value<int> orderIndex,
      Value<int> rowid,
    });

final class $$TracksTableReferences
    extends BaseReferences<_$AppDatabase, $TracksTable, TrackRow> {
  $$TracksTableReferences(super.$_db, super.$_table, super.$_typedResult);

  static $PracticesTable _practiceIdTable(_$AppDatabase db) =>
      db.practices.createAlias('tracks__practice_id__practices__id');

  $$PracticesTableProcessedTableManager get practiceId {
    final $_column = $_itemColumn<String>('practice_id')!;

    final manager = $$PracticesTableTableManager(
      $_db,
      $_db.practices,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_practiceIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static $InterpretationsTable _interpretationIdTable(_$AppDatabase db) => db
      .interpretations
      .createAlias('tracks__interpretation_id__interpretations__id');

  $$InterpretationsTableProcessedTableManager? get interpretationId {
    final $_column = $_itemColumn<String>('interpretation_id');
    if ($_column == null) return null;
    final manager = $$InterpretationsTableTableManager(
      $_db,
      $_db.interpretations,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_interpretationIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static $FxBusesTable _busIdTable(_$AppDatabase db) =>
      db.fxBuses.createAlias('tracks__bus_id__fx_buses__id');

  $$FxBusesTableProcessedTableManager? get busId {
    final $_column = $_itemColumn<String>('bus_id');
    if ($_column == null) return null;
    final manager = $$FxBusesTableTableManager(
      $_db,
      $_db.fxBuses,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_busIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static MultiTypedResultKey<$ClipsTable, List<ClipRow>> _clipsRefsTable(
    _$AppDatabase db,
  ) => MultiTypedResultKey.fromTable(
    db.clips,
    aliasName: 'tracks__id__clips__track_id',
  );

  $$ClipsTableProcessedTableManager get clipsRefs {
    final manager = $$ClipsTableTableManager(
      $_db,
      $_db.clips,
    ).filter((f) => f.trackId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_clipsRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }

  static MultiTypedResultKey<$PracticeNotesTable, List<PracticeNoteRow>>
  _practiceNotesRefsTable(_$AppDatabase db) => MultiTypedResultKey.fromTable(
    db.practiceNotes,
    aliasName: 'tracks__id__practice_notes__track_id',
  );

  $$PracticeNotesTableProcessedTableManager get practiceNotesRefs {
    final manager = $$PracticeNotesTableTableManager(
      $_db,
      $_db.practiceNotes,
    ).filter((f) => f.trackId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_practiceNotesRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }

  static MultiTypedResultKey<$TechniqueMarkersTable, List<TechniqueMarkerRow>>
  _techniqueMarkersRefsTable(_$AppDatabase db) => MultiTypedResultKey.fromTable(
    db.techniqueMarkers,
    aliasName: 'tracks__id__technique_markers__track_id',
  );

  $$TechniqueMarkersTableProcessedTableManager get techniqueMarkersRefs {
    final manager = $$TechniqueMarkersTableTableManager(
      $_db,
      $_db.techniqueMarkers,
    ).filter((f) => f.trackId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(
      _techniqueMarkersRefsTable($_db),
    );
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }
}

class $$TracksTableFilterComposer
    extends Composer<_$AppDatabase, $TracksTable> {
  $$TracksTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get kind => $composableBuilder(
    column: $table.kind,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get displayName => $composableBuilder(
    column: $table.displayName,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get colorValue => $composableBuilder(
    column: $table.colorValue,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<bool> get muted => $composableBuilder(
    column: $table.muted,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<bool> get soloed => $composableBuilder(
    column: $table.soloed,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<double> get volume => $composableBuilder(
    column: $table.volume,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<double> get pan => $composableBuilder(
    column: $table.pan,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get fxJson => $composableBuilder(
    column: $table.fxJson,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get sendsJson => $composableBuilder(
    column: $table.sendsJson,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnFilters(column),
  );

  $$PracticesTableFilterComposer get practiceId {
    final $$PracticesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableFilterComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$InterpretationsTableFilterComposer get interpretationId {
    final $$InterpretationsTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.interpretationId,
      referencedTable: $db.interpretations,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$InterpretationsTableFilterComposer(
            $db: $db,
            $table: $db.interpretations,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$FxBusesTableFilterComposer get busId {
    final $$FxBusesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.busId,
      referencedTable: $db.fxBuses,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$FxBusesTableFilterComposer(
            $db: $db,
            $table: $db.fxBuses,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<bool> clipsRefs(
    Expression<bool> Function($$ClipsTableFilterComposer f) f,
  ) {
    final $$ClipsTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.clips,
      getReferencedColumn: (t) => t.trackId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$ClipsTableFilterComposer(
            $db: $db,
            $table: $db.clips,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<bool> practiceNotesRefs(
    Expression<bool> Function($$PracticeNotesTableFilterComposer f) f,
  ) {
    final $$PracticeNotesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.practiceNotes,
      getReferencedColumn: (t) => t.trackId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticeNotesTableFilterComposer(
            $db: $db,
            $table: $db.practiceNotes,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<bool> techniqueMarkersRefs(
    Expression<bool> Function($$TechniqueMarkersTableFilterComposer f) f,
  ) {
    final $$TechniqueMarkersTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.techniqueMarkers,
      getReferencedColumn: (t) => t.trackId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniqueMarkersTableFilterComposer(
            $db: $db,
            $table: $db.techniqueMarkers,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$TracksTableOrderingComposer
    extends Composer<_$AppDatabase, $TracksTable> {
  $$TracksTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get kind => $composableBuilder(
    column: $table.kind,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get displayName => $composableBuilder(
    column: $table.displayName,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get colorValue => $composableBuilder(
    column: $table.colorValue,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<bool> get muted => $composableBuilder(
    column: $table.muted,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<bool> get soloed => $composableBuilder(
    column: $table.soloed,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<double> get volume => $composableBuilder(
    column: $table.volume,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<double> get pan => $composableBuilder(
    column: $table.pan,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get fxJson => $composableBuilder(
    column: $table.fxJson,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get sendsJson => $composableBuilder(
    column: $table.sendsJson,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnOrderings(column),
  );

  $$PracticesTableOrderingComposer get practiceId {
    final $$PracticesTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableOrderingComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$InterpretationsTableOrderingComposer get interpretationId {
    final $$InterpretationsTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.interpretationId,
      referencedTable: $db.interpretations,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$InterpretationsTableOrderingComposer(
            $db: $db,
            $table: $db.interpretations,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$FxBusesTableOrderingComposer get busId {
    final $$FxBusesTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.busId,
      referencedTable: $db.fxBuses,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$FxBusesTableOrderingComposer(
            $db: $db,
            $table: $db.fxBuses,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$TracksTableAnnotationComposer
    extends Composer<_$AppDatabase, $TracksTable> {
  $$TracksTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<String> get kind =>
      $composableBuilder(column: $table.kind, builder: (column) => column);

  GeneratedColumn<String> get displayName => $composableBuilder(
    column: $table.displayName,
    builder: (column) => column,
  );

  GeneratedColumn<int> get colorValue => $composableBuilder(
    column: $table.colorValue,
    builder: (column) => column,
  );

  GeneratedColumn<bool> get muted =>
      $composableBuilder(column: $table.muted, builder: (column) => column);

  GeneratedColumn<bool> get soloed =>
      $composableBuilder(column: $table.soloed, builder: (column) => column);

  GeneratedColumn<double> get volume =>
      $composableBuilder(column: $table.volume, builder: (column) => column);

  GeneratedColumn<double> get pan =>
      $composableBuilder(column: $table.pan, builder: (column) => column);

  GeneratedColumn<String> get fxJson =>
      $composableBuilder(column: $table.fxJson, builder: (column) => column);

  GeneratedColumn<String> get sendsJson =>
      $composableBuilder(column: $table.sendsJson, builder: (column) => column);

  GeneratedColumn<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => column,
  );

  $$PracticesTableAnnotationComposer get practiceId {
    final $$PracticesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.practiceId,
      referencedTable: $db.practices,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticesTableAnnotationComposer(
            $db: $db,
            $table: $db.practices,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$InterpretationsTableAnnotationComposer get interpretationId {
    final $$InterpretationsTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.interpretationId,
      referencedTable: $db.interpretations,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$InterpretationsTableAnnotationComposer(
            $db: $db,
            $table: $db.interpretations,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$FxBusesTableAnnotationComposer get busId {
    final $$FxBusesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.busId,
      referencedTable: $db.fxBuses,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$FxBusesTableAnnotationComposer(
            $db: $db,
            $table: $db.fxBuses,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  Expression<T> clipsRefs<T extends Object>(
    Expression<T> Function($$ClipsTableAnnotationComposer a) f,
  ) {
    final $$ClipsTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.clips,
      getReferencedColumn: (t) => t.trackId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$ClipsTableAnnotationComposer(
            $db: $db,
            $table: $db.clips,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<T> practiceNotesRefs<T extends Object>(
    Expression<T> Function($$PracticeNotesTableAnnotationComposer a) f,
  ) {
    final $$PracticeNotesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.practiceNotes,
      getReferencedColumn: (t) => t.trackId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$PracticeNotesTableAnnotationComposer(
            $db: $db,
            $table: $db.practiceNotes,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }

  Expression<T> techniqueMarkersRefs<T extends Object>(
    Expression<T> Function($$TechniqueMarkersTableAnnotationComposer a) f,
  ) {
    final $$TechniqueMarkersTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.techniqueMarkers,
      getReferencedColumn: (t) => t.trackId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniqueMarkersTableAnnotationComposer(
            $db: $db,
            $table: $db.techniqueMarkers,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$TracksTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $TracksTable,
          TrackRow,
          $$TracksTableFilterComposer,
          $$TracksTableOrderingComposer,
          $$TracksTableAnnotationComposer,
          $$TracksTableCreateCompanionBuilder,
          $$TracksTableUpdateCompanionBuilder,
          (TrackRow, $$TracksTableReferences),
          TrackRow,
          PrefetchHooks Function({
            bool practiceId,
            bool interpretationId,
            bool busId,
            bool clipsRefs,
            bool practiceNotesRefs,
            bool techniqueMarkersRefs,
          })
        > {
  $$TracksTableTableManager(_$AppDatabase db, $TracksTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$TracksTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$TracksTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$TracksTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> practiceId = const Value.absent(),
                Value<String> kind = const Value.absent(),
                Value<String?> interpretationId = const Value.absent(),
                Value<String> displayName = const Value.absent(),
                Value<int?> colorValue = const Value.absent(),
                Value<bool> muted = const Value.absent(),
                Value<bool> soloed = const Value.absent(),
                Value<double> volume = const Value.absent(),
                Value<double> pan = const Value.absent(),
                Value<String?> busId = const Value.absent(),
                Value<String?> fxJson = const Value.absent(),
                Value<String?> sendsJson = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => TracksCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                practiceId: practiceId,
                kind: kind,
                interpretationId: interpretationId,
                displayName: displayName,
                colorValue: colorValue,
                muted: muted,
                soloed: soloed,
                volume: volume,
                pan: pan,
                busId: busId,
                fxJson: fxJson,
                sendsJson: sendsJson,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String practiceId,
                required String kind,
                Value<String?> interpretationId = const Value.absent(),
                Value<String> displayName = const Value.absent(),
                Value<int?> colorValue = const Value.absent(),
                Value<bool> muted = const Value.absent(),
                Value<bool> soloed = const Value.absent(),
                Value<double> volume = const Value.absent(),
                Value<double> pan = const Value.absent(),
                Value<String?> busId = const Value.absent(),
                Value<String?> fxJson = const Value.absent(),
                Value<String?> sendsJson = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => TracksCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                practiceId: practiceId,
                kind: kind,
                interpretationId: interpretationId,
                displayName: displayName,
                colorValue: colorValue,
                muted: muted,
                soloed: soloed,
                volume: volume,
                pan: pan,
                busId: busId,
                fxJson: fxJson,
                sendsJson: sendsJson,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) =>
                    (e.readTable(table), $$TracksTableReferences(db, table, e)),
              )
              .toList(),
          prefetchHooksCallback:
              ({
                practiceId = false,
                interpretationId = false,
                busId = false,
                clipsRefs = false,
                practiceNotesRefs = false,
                techniqueMarkersRefs = false,
              }) {
                return PrefetchHooks(
                  db: db,
                  explicitlyWatchedTables: [
                    if (clipsRefs) db.clips,
                    if (practiceNotesRefs) db.practiceNotes,
                    if (techniqueMarkersRefs) db.techniqueMarkers,
                  ],
                  addJoins:
                      <
                        T extends TableManagerState<
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic,
                          dynamic
                        >
                      >(state) {
                        if (practiceId) {
                          state =
                              state.withJoin(
                                    currentTable: table,
                                    currentColumn: table.practiceId,
                                    referencedTable: $$TracksTableReferences
                                        ._practiceIdTable(db),
                                    referencedColumn: $$TracksTableReferences
                                        ._practiceIdTable(db)
                                        .id,
                                  )
                                  as T;
                        }
                        if (interpretationId) {
                          state =
                              state.withJoin(
                                    currentTable: table,
                                    currentColumn: table.interpretationId,
                                    referencedTable: $$TracksTableReferences
                                        ._interpretationIdTable(db),
                                    referencedColumn: $$TracksTableReferences
                                        ._interpretationIdTable(db)
                                        .id,
                                  )
                                  as T;
                        }
                        if (busId) {
                          state =
                              state.withJoin(
                                    currentTable: table,
                                    currentColumn: table.busId,
                                    referencedTable: $$TracksTableReferences
                                        ._busIdTable(db),
                                    referencedColumn: $$TracksTableReferences
                                        ._busIdTable(db)
                                        .id,
                                  )
                                  as T;
                        }

                        return state;
                      },
                  getPrefetchedDataCallback: (items) async {
                    return [
                      if (clipsRefs)
                        await $_getPrefetchedData<
                          TrackRow,
                          $TracksTable,
                          ClipRow
                        >(
                          currentTable: table,
                          referencedTable: $$TracksTableReferences
                              ._clipsRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$TracksTableReferences(db, table, p0).clipsRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.trackId == item.id,
                              ),
                          typedResults: items,
                        ),
                      if (practiceNotesRefs)
                        await $_getPrefetchedData<
                          TrackRow,
                          $TracksTable,
                          PracticeNoteRow
                        >(
                          currentTable: table,
                          referencedTable: $$TracksTableReferences
                              ._practiceNotesRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$TracksTableReferences(
                                db,
                                table,
                                p0,
                              ).practiceNotesRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.trackId == item.id,
                              ),
                          typedResults: items,
                        ),
                      if (techniqueMarkersRefs)
                        await $_getPrefetchedData<
                          TrackRow,
                          $TracksTable,
                          TechniqueMarkerRow
                        >(
                          currentTable: table,
                          referencedTable: $$TracksTableReferences
                              ._techniqueMarkersRefsTable(db),
                          managerFromTypedResult: (p0) =>
                              $$TracksTableReferences(
                                db,
                                table,
                                p0,
                              ).techniqueMarkersRefs,
                          referencedItemsForCurrentItem:
                              (item, referencedItems) => referencedItems.where(
                                (e) => e.trackId == item.id,
                              ),
                          typedResults: items,
                        ),
                    ];
                  },
                );
              },
        ),
      );
}

typedef $$TracksTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $TracksTable,
      TrackRow,
      $$TracksTableFilterComposer,
      $$TracksTableOrderingComposer,
      $$TracksTableAnnotationComposer,
      $$TracksTableCreateCompanionBuilder,
      $$TracksTableUpdateCompanionBuilder,
      (TrackRow, $$TracksTableReferences),
      TrackRow,
      PrefetchHooks Function({
        bool practiceId,
        bool interpretationId,
        bool busId,
        bool clipsRefs,
        bool practiceNotesRefs,
        bool techniqueMarkersRefs,
      })
    >;
typedef $$ClipsTableCreateCompanionBuilder =
    ClipsCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String trackId,
      required String assetId,
      Value<int> startMs,
      Value<int> durationMs,
      Value<int> orderIndex,
      Value<int> rowid,
    });
typedef $$ClipsTableUpdateCompanionBuilder =
    ClipsCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> trackId,
      Value<String> assetId,
      Value<int> startMs,
      Value<int> durationMs,
      Value<int> orderIndex,
      Value<int> rowid,
    });

final class $$ClipsTableReferences
    extends BaseReferences<_$AppDatabase, $ClipsTable, ClipRow> {
  $$ClipsTableReferences(super.$_db, super.$_table, super.$_typedResult);

  static $TracksTable _trackIdTable(_$AppDatabase db) =>
      db.tracks.createAlias('clips__track_id__tracks__id');

  $$TracksTableProcessedTableManager get trackId {
    final $_column = $_itemColumn<String>('track_id')!;

    final manager = $$TracksTableTableManager(
      $_db,
      $_db.tracks,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_trackIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static $AssetsTable _assetIdTable(_$AppDatabase db) =>
      db.assets.createAlias('clips__asset_id__assets__id');

  $$AssetsTableProcessedTableManager get assetId {
    final $_column = $_itemColumn<String>('asset_id')!;

    final manager = $$AssetsTableTableManager(
      $_db,
      $_db.assets,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_assetIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }
}

class $$ClipsTableFilterComposer extends Composer<_$AppDatabase, $ClipsTable> {
  $$ClipsTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get startMs => $composableBuilder(
    column: $table.startMs,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get durationMs => $composableBuilder(
    column: $table.durationMs,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnFilters(column),
  );

  $$TracksTableFilterComposer get trackId {
    final $$TracksTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableFilterComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$AssetsTableFilterComposer get assetId {
    final $$AssetsTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.assetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableFilterComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$ClipsTableOrderingComposer
    extends Composer<_$AppDatabase, $ClipsTable> {
  $$ClipsTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get startMs => $composableBuilder(
    column: $table.startMs,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get durationMs => $composableBuilder(
    column: $table.durationMs,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnOrderings(column),
  );

  $$TracksTableOrderingComposer get trackId {
    final $$TracksTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableOrderingComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$AssetsTableOrderingComposer get assetId {
    final $$AssetsTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.assetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableOrderingComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$ClipsTableAnnotationComposer
    extends Composer<_$AppDatabase, $ClipsTable> {
  $$ClipsTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<int> get startMs =>
      $composableBuilder(column: $table.startMs, builder: (column) => column);

  GeneratedColumn<int> get durationMs => $composableBuilder(
    column: $table.durationMs,
    builder: (column) => column,
  );

  GeneratedColumn<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => column,
  );

  $$TracksTableAnnotationComposer get trackId {
    final $$TracksTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableAnnotationComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$AssetsTableAnnotationComposer get assetId {
    final $$AssetsTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.assetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableAnnotationComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$ClipsTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $ClipsTable,
          ClipRow,
          $$ClipsTableFilterComposer,
          $$ClipsTableOrderingComposer,
          $$ClipsTableAnnotationComposer,
          $$ClipsTableCreateCompanionBuilder,
          $$ClipsTableUpdateCompanionBuilder,
          (ClipRow, $$ClipsTableReferences),
          ClipRow,
          PrefetchHooks Function({bool trackId, bool assetId})
        > {
  $$ClipsTableTableManager(_$AppDatabase db, $ClipsTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$ClipsTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$ClipsTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$ClipsTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> trackId = const Value.absent(),
                Value<String> assetId = const Value.absent(),
                Value<int> startMs = const Value.absent(),
                Value<int> durationMs = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => ClipsCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                trackId: trackId,
                assetId: assetId,
                startMs: startMs,
                durationMs: durationMs,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String trackId,
                required String assetId,
                Value<int> startMs = const Value.absent(),
                Value<int> durationMs = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => ClipsCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                trackId: trackId,
                assetId: assetId,
                startMs: startMs,
                durationMs: durationMs,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) =>
                    (e.readTable(table), $$ClipsTableReferences(db, table, e)),
              )
              .toList(),
          prefetchHooksCallback: ({trackId = false, assetId = false}) {
            return PrefetchHooks(
              db: db,
              explicitlyWatchedTables: [],
              addJoins:
                  <
                    T extends TableManagerState<
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic
                    >
                  >(state) {
                    if (trackId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.trackId,
                                referencedTable: $$ClipsTableReferences
                                    ._trackIdTable(db),
                                referencedColumn: $$ClipsTableReferences
                                    ._trackIdTable(db)
                                    .id,
                              )
                              as T;
                    }
                    if (assetId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.assetId,
                                referencedTable: $$ClipsTableReferences
                                    ._assetIdTable(db),
                                referencedColumn: $$ClipsTableReferences
                                    ._assetIdTable(db)
                                    .id,
                              )
                              as T;
                    }

                    return state;
                  },
              getPrefetchedDataCallback: (items) async {
                return [];
              },
            );
          },
        ),
      );
}

typedef $$ClipsTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $ClipsTable,
      ClipRow,
      $$ClipsTableFilterComposer,
      $$ClipsTableOrderingComposer,
      $$ClipsTableAnnotationComposer,
      $$ClipsTableCreateCompanionBuilder,
      $$ClipsTableUpdateCompanionBuilder,
      (ClipRow, $$ClipsTableReferences),
      ClipRow,
      PrefetchHooks Function({bool trackId, bool assetId})
    >;
typedef $$PracticeNotesTableCreateCompanionBuilder =
    PracticeNotesCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String trackId,
      Value<int> positionMs,
      Value<int?> endMs,
      Value<String> content,
      Value<int> rowid,
    });
typedef $$PracticeNotesTableUpdateCompanionBuilder =
    PracticeNotesCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> trackId,
      Value<int> positionMs,
      Value<int?> endMs,
      Value<String> content,
      Value<int> rowid,
    });

final class $$PracticeNotesTableReferences
    extends
        BaseReferences<_$AppDatabase, $PracticeNotesTable, PracticeNoteRow> {
  $$PracticeNotesTableReferences(
    super.$_db,
    super.$_table,
    super.$_typedResult,
  );

  static $TracksTable _trackIdTable(_$AppDatabase db) =>
      db.tracks.createAlias('practice_notes__track_id__tracks__id');

  $$TracksTableProcessedTableManager get trackId {
    final $_column = $_itemColumn<String>('track_id')!;

    final manager = $$TracksTableTableManager(
      $_db,
      $_db.tracks,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_trackIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }
}

class $$PracticeNotesTableFilterComposer
    extends Composer<_$AppDatabase, $PracticeNotesTable> {
  $$PracticeNotesTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get positionMs => $composableBuilder(
    column: $table.positionMs,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get endMs => $composableBuilder(
    column: $table.endMs,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get content => $composableBuilder(
    column: $table.content,
    builder: (column) => ColumnFilters(column),
  );

  $$TracksTableFilterComposer get trackId {
    final $$TracksTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableFilterComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$PracticeNotesTableOrderingComposer
    extends Composer<_$AppDatabase, $PracticeNotesTable> {
  $$PracticeNotesTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get positionMs => $composableBuilder(
    column: $table.positionMs,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get endMs => $composableBuilder(
    column: $table.endMs,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get content => $composableBuilder(
    column: $table.content,
    builder: (column) => ColumnOrderings(column),
  );

  $$TracksTableOrderingComposer get trackId {
    final $$TracksTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableOrderingComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$PracticeNotesTableAnnotationComposer
    extends Composer<_$AppDatabase, $PracticeNotesTable> {
  $$PracticeNotesTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<int> get positionMs => $composableBuilder(
    column: $table.positionMs,
    builder: (column) => column,
  );

  GeneratedColumn<int> get endMs =>
      $composableBuilder(column: $table.endMs, builder: (column) => column);

  GeneratedColumn<String> get content =>
      $composableBuilder(column: $table.content, builder: (column) => column);

  $$TracksTableAnnotationComposer get trackId {
    final $$TracksTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableAnnotationComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$PracticeNotesTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $PracticeNotesTable,
          PracticeNoteRow,
          $$PracticeNotesTableFilterComposer,
          $$PracticeNotesTableOrderingComposer,
          $$PracticeNotesTableAnnotationComposer,
          $$PracticeNotesTableCreateCompanionBuilder,
          $$PracticeNotesTableUpdateCompanionBuilder,
          (PracticeNoteRow, $$PracticeNotesTableReferences),
          PracticeNoteRow,
          PrefetchHooks Function({bool trackId})
        > {
  $$PracticeNotesTableTableManager(_$AppDatabase db, $PracticeNotesTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$PracticeNotesTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$PracticeNotesTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$PracticeNotesTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> trackId = const Value.absent(),
                Value<int> positionMs = const Value.absent(),
                Value<int?> endMs = const Value.absent(),
                Value<String> content = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => PracticeNotesCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                trackId: trackId,
                positionMs: positionMs,
                endMs: endMs,
                content: content,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String trackId,
                Value<int> positionMs = const Value.absent(),
                Value<int?> endMs = const Value.absent(),
                Value<String> content = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => PracticeNotesCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                trackId: trackId,
                positionMs: positionMs,
                endMs: endMs,
                content: content,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) => (
                  e.readTable(table),
                  $$PracticeNotesTableReferences(db, table, e),
                ),
              )
              .toList(),
          prefetchHooksCallback: ({trackId = false}) {
            return PrefetchHooks(
              db: db,
              explicitlyWatchedTables: [],
              addJoins:
                  <
                    T extends TableManagerState<
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic
                    >
                  >(state) {
                    if (trackId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.trackId,
                                referencedTable: $$PracticeNotesTableReferences
                                    ._trackIdTable(db),
                                referencedColumn: $$PracticeNotesTableReferences
                                    ._trackIdTable(db)
                                    .id,
                              )
                              as T;
                    }

                    return state;
                  },
              getPrefetchedDataCallback: (items) async {
                return [];
              },
            );
          },
        ),
      );
}

typedef $$PracticeNotesTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $PracticeNotesTable,
      PracticeNoteRow,
      $$PracticeNotesTableFilterComposer,
      $$PracticeNotesTableOrderingComposer,
      $$PracticeNotesTableAnnotationComposer,
      $$PracticeNotesTableCreateCompanionBuilder,
      $$PracticeNotesTableUpdateCompanionBuilder,
      (PracticeNoteRow, $$PracticeNotesTableReferences),
      PracticeNoteRow,
      PrefetchHooks Function({bool trackId})
    >;
typedef $$TechniqueMarkersTableCreateCompanionBuilder =
    TechniqueMarkersCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String trackId,
      Value<int> positionMs,
      Value<int?> endMs,
      required String techniqueId,
      required String techniqueNameSnapshot,
      Value<int> rowid,
    });
typedef $$TechniqueMarkersTableUpdateCompanionBuilder =
    TechniqueMarkersCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> trackId,
      Value<int> positionMs,
      Value<int?> endMs,
      Value<String> techniqueId,
      Value<String> techniqueNameSnapshot,
      Value<int> rowid,
    });

final class $$TechniqueMarkersTableReferences
    extends
        BaseReferences<
          _$AppDatabase,
          $TechniqueMarkersTable,
          TechniqueMarkerRow
        > {
  $$TechniqueMarkersTableReferences(
    super.$_db,
    super.$_table,
    super.$_typedResult,
  );

  static $TracksTable _trackIdTable(_$AppDatabase db) =>
      db.tracks.createAlias('technique_markers__track_id__tracks__id');

  $$TracksTableProcessedTableManager get trackId {
    final $_column = $_itemColumn<String>('track_id')!;

    final manager = $$TracksTableTableManager(
      $_db,
      $_db.tracks,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_trackIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }
}

class $$TechniqueMarkersTableFilterComposer
    extends Composer<_$AppDatabase, $TechniqueMarkersTable> {
  $$TechniqueMarkersTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get positionMs => $composableBuilder(
    column: $table.positionMs,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get endMs => $composableBuilder(
    column: $table.endMs,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get techniqueId => $composableBuilder(
    column: $table.techniqueId,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get techniqueNameSnapshot => $composableBuilder(
    column: $table.techniqueNameSnapshot,
    builder: (column) => ColumnFilters(column),
  );

  $$TracksTableFilterComposer get trackId {
    final $$TracksTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableFilterComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$TechniqueMarkersTableOrderingComposer
    extends Composer<_$AppDatabase, $TechniqueMarkersTable> {
  $$TechniqueMarkersTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get positionMs => $composableBuilder(
    column: $table.positionMs,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get endMs => $composableBuilder(
    column: $table.endMs,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get techniqueId => $composableBuilder(
    column: $table.techniqueId,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get techniqueNameSnapshot => $composableBuilder(
    column: $table.techniqueNameSnapshot,
    builder: (column) => ColumnOrderings(column),
  );

  $$TracksTableOrderingComposer get trackId {
    final $$TracksTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableOrderingComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$TechniqueMarkersTableAnnotationComposer
    extends Composer<_$AppDatabase, $TechniqueMarkersTable> {
  $$TechniqueMarkersTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<int> get positionMs => $composableBuilder(
    column: $table.positionMs,
    builder: (column) => column,
  );

  GeneratedColumn<int> get endMs =>
      $composableBuilder(column: $table.endMs, builder: (column) => column);

  GeneratedColumn<String> get techniqueId => $composableBuilder(
    column: $table.techniqueId,
    builder: (column) => column,
  );

  GeneratedColumn<String> get techniqueNameSnapshot => $composableBuilder(
    column: $table.techniqueNameSnapshot,
    builder: (column) => column,
  );

  $$TracksTableAnnotationComposer get trackId {
    final $$TracksTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.trackId,
      referencedTable: $db.tracks,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TracksTableAnnotationComposer(
            $db: $db,
            $table: $db.tracks,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$TechniqueMarkersTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $TechniqueMarkersTable,
          TechniqueMarkerRow,
          $$TechniqueMarkersTableFilterComposer,
          $$TechniqueMarkersTableOrderingComposer,
          $$TechniqueMarkersTableAnnotationComposer,
          $$TechniqueMarkersTableCreateCompanionBuilder,
          $$TechniqueMarkersTableUpdateCompanionBuilder,
          (TechniqueMarkerRow, $$TechniqueMarkersTableReferences),
          TechniqueMarkerRow,
          PrefetchHooks Function({bool trackId})
        > {
  $$TechniqueMarkersTableTableManager(
    _$AppDatabase db,
    $TechniqueMarkersTable table,
  ) : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$TechniqueMarkersTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$TechniqueMarkersTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$TechniqueMarkersTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> trackId = const Value.absent(),
                Value<int> positionMs = const Value.absent(),
                Value<int?> endMs = const Value.absent(),
                Value<String> techniqueId = const Value.absent(),
                Value<String> techniqueNameSnapshot = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => TechniqueMarkersCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                trackId: trackId,
                positionMs: positionMs,
                endMs: endMs,
                techniqueId: techniqueId,
                techniqueNameSnapshot: techniqueNameSnapshot,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String trackId,
                Value<int> positionMs = const Value.absent(),
                Value<int?> endMs = const Value.absent(),
                required String techniqueId,
                required String techniqueNameSnapshot,
                Value<int> rowid = const Value.absent(),
              }) => TechniqueMarkersCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                trackId: trackId,
                positionMs: positionMs,
                endMs: endMs,
                techniqueId: techniqueId,
                techniqueNameSnapshot: techniqueNameSnapshot,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) => (
                  e.readTable(table),
                  $$TechniqueMarkersTableReferences(db, table, e),
                ),
              )
              .toList(),
          prefetchHooksCallback: ({trackId = false}) {
            return PrefetchHooks(
              db: db,
              explicitlyWatchedTables: [],
              addJoins:
                  <
                    T extends TableManagerState<
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic
                    >
                  >(state) {
                    if (trackId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.trackId,
                                referencedTable:
                                    $$TechniqueMarkersTableReferences
                                        ._trackIdTable(db),
                                referencedColumn:
                                    $$TechniqueMarkersTableReferences
                                        ._trackIdTable(db)
                                        .id,
                              )
                              as T;
                    }

                    return state;
                  },
              getPrefetchedDataCallback: (items) async {
                return [];
              },
            );
          },
        ),
      );
}

typedef $$TechniqueMarkersTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $TechniqueMarkersTable,
      TechniqueMarkerRow,
      $$TechniqueMarkersTableFilterComposer,
      $$TechniqueMarkersTableOrderingComposer,
      $$TechniqueMarkersTableAnnotationComposer,
      $$TechniqueMarkersTableCreateCompanionBuilder,
      $$TechniqueMarkersTableUpdateCompanionBuilder,
      (TechniqueMarkerRow, $$TechniqueMarkersTableReferences),
      TechniqueMarkerRow,
      PrefetchHooks Function({bool trackId})
    >;
typedef $$TechniquesTableCreateCompanionBuilder =
    TechniquesCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      Value<String> name,
      Value<String?> aliasesJson,
      Value<String?> tagsJson,
      Value<String> summary,
      Value<String> description,
      Value<String> cues,
      Value<String> exercises,
      Value<String> acousticSignature,
      Value<int> orderIndex,
      Value<int> rowid,
    });
typedef $$TechniquesTableUpdateCompanionBuilder =
    TechniquesCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> name,
      Value<String?> aliasesJson,
      Value<String?> tagsJson,
      Value<String> summary,
      Value<String> description,
      Value<String> cues,
      Value<String> exercises,
      Value<String> acousticSignature,
      Value<int> orderIndex,
      Value<int> rowid,
    });

final class $$TechniquesTableReferences
    extends BaseReferences<_$AppDatabase, $TechniquesTable, TechniqueRow> {
  $$TechniquesTableReferences(super.$_db, super.$_table, super.$_typedResult);

  static MultiTypedResultKey<$TechniqueMediaTable, List<TechniqueMediaRow>>
  _techniqueMediaRefsTable(_$AppDatabase db) => MultiTypedResultKey.fromTable(
    db.techniqueMedia,
    aliasName: 'techniques__id__technique_media__technique_id',
  );

  $$TechniqueMediaTableProcessedTableManager get techniqueMediaRefs {
    final manager = $$TechniqueMediaTableTableManager(
      $_db,
      $_db.techniqueMedia,
    ).filter((f) => f.techniqueId.id.sqlEquals($_itemColumn<String>('id')!));

    final cache = $_typedResult.readTableOrNull(_techniqueMediaRefsTable($_db));
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: cache),
    );
  }
}

class $$TechniquesTableFilterComposer
    extends Composer<_$AppDatabase, $TechniquesTable> {
  $$TechniquesTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get name => $composableBuilder(
    column: $table.name,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get aliasesJson => $composableBuilder(
    column: $table.aliasesJson,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get tagsJson => $composableBuilder(
    column: $table.tagsJson,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get summary => $composableBuilder(
    column: $table.summary,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get description => $composableBuilder(
    column: $table.description,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get cues => $composableBuilder(
    column: $table.cues,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get exercises => $composableBuilder(
    column: $table.exercises,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get acousticSignature => $composableBuilder(
    column: $table.acousticSignature,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnFilters(column),
  );

  Expression<bool> techniqueMediaRefs(
    Expression<bool> Function($$TechniqueMediaTableFilterComposer f) f,
  ) {
    final $$TechniqueMediaTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.techniqueMedia,
      getReferencedColumn: (t) => t.techniqueId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniqueMediaTableFilterComposer(
            $db: $db,
            $table: $db.techniqueMedia,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$TechniquesTableOrderingComposer
    extends Composer<_$AppDatabase, $TechniquesTable> {
  $$TechniquesTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get name => $composableBuilder(
    column: $table.name,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get aliasesJson => $composableBuilder(
    column: $table.aliasesJson,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get tagsJson => $composableBuilder(
    column: $table.tagsJson,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get summary => $composableBuilder(
    column: $table.summary,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get description => $composableBuilder(
    column: $table.description,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get cues => $composableBuilder(
    column: $table.cues,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get exercises => $composableBuilder(
    column: $table.exercises,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get acousticSignature => $composableBuilder(
    column: $table.acousticSignature,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnOrderings(column),
  );
}

class $$TechniquesTableAnnotationComposer
    extends Composer<_$AppDatabase, $TechniquesTable> {
  $$TechniquesTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<String> get name =>
      $composableBuilder(column: $table.name, builder: (column) => column);

  GeneratedColumn<String> get aliasesJson => $composableBuilder(
    column: $table.aliasesJson,
    builder: (column) => column,
  );

  GeneratedColumn<String> get tagsJson =>
      $composableBuilder(column: $table.tagsJson, builder: (column) => column);

  GeneratedColumn<String> get summary =>
      $composableBuilder(column: $table.summary, builder: (column) => column);

  GeneratedColumn<String> get description => $composableBuilder(
    column: $table.description,
    builder: (column) => column,
  );

  GeneratedColumn<String> get cues =>
      $composableBuilder(column: $table.cues, builder: (column) => column);

  GeneratedColumn<String> get exercises =>
      $composableBuilder(column: $table.exercises, builder: (column) => column);

  GeneratedColumn<String> get acousticSignature => $composableBuilder(
    column: $table.acousticSignature,
    builder: (column) => column,
  );

  GeneratedColumn<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => column,
  );

  Expression<T> techniqueMediaRefs<T extends Object>(
    Expression<T> Function($$TechniqueMediaTableAnnotationComposer a) f,
  ) {
    final $$TechniqueMediaTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.id,
      referencedTable: $db.techniqueMedia,
      getReferencedColumn: (t) => t.techniqueId,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniqueMediaTableAnnotationComposer(
            $db: $db,
            $table: $db.techniqueMedia,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return f(composer);
  }
}

class $$TechniquesTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $TechniquesTable,
          TechniqueRow,
          $$TechniquesTableFilterComposer,
          $$TechniquesTableOrderingComposer,
          $$TechniquesTableAnnotationComposer,
          $$TechniquesTableCreateCompanionBuilder,
          $$TechniquesTableUpdateCompanionBuilder,
          (TechniqueRow, $$TechniquesTableReferences),
          TechniqueRow,
          PrefetchHooks Function({bool techniqueMediaRefs})
        > {
  $$TechniquesTableTableManager(_$AppDatabase db, $TechniquesTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$TechniquesTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$TechniquesTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$TechniquesTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> name = const Value.absent(),
                Value<String?> aliasesJson = const Value.absent(),
                Value<String?> tagsJson = const Value.absent(),
                Value<String> summary = const Value.absent(),
                Value<String> description = const Value.absent(),
                Value<String> cues = const Value.absent(),
                Value<String> exercises = const Value.absent(),
                Value<String> acousticSignature = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => TechniquesCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                name: name,
                aliasesJson: aliasesJson,
                tagsJson: tagsJson,
                summary: summary,
                description: description,
                cues: cues,
                exercises: exercises,
                acousticSignature: acousticSignature,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                Value<String> name = const Value.absent(),
                Value<String?> aliasesJson = const Value.absent(),
                Value<String?> tagsJson = const Value.absent(),
                Value<String> summary = const Value.absent(),
                Value<String> description = const Value.absent(),
                Value<String> cues = const Value.absent(),
                Value<String> exercises = const Value.absent(),
                Value<String> acousticSignature = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => TechniquesCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                name: name,
                aliasesJson: aliasesJson,
                tagsJson: tagsJson,
                summary: summary,
                description: description,
                cues: cues,
                exercises: exercises,
                acousticSignature: acousticSignature,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) => (
                  e.readTable(table),
                  $$TechniquesTableReferences(db, table, e),
                ),
              )
              .toList(),
          prefetchHooksCallback: ({techniqueMediaRefs = false}) {
            return PrefetchHooks(
              db: db,
              explicitlyWatchedTables: [
                if (techniqueMediaRefs) db.techniqueMedia,
              ],
              addJoins: null,
              getPrefetchedDataCallback: (items) async {
                return [
                  if (techniqueMediaRefs)
                    await $_getPrefetchedData<
                      TechniqueRow,
                      $TechniquesTable,
                      TechniqueMediaRow
                    >(
                      currentTable: table,
                      referencedTable: $$TechniquesTableReferences
                          ._techniqueMediaRefsTable(db),
                      managerFromTypedResult: (p0) =>
                          $$TechniquesTableReferences(
                            db,
                            table,
                            p0,
                          ).techniqueMediaRefs,
                      referencedItemsForCurrentItem: (item, referencedItems) =>
                          referencedItems.where(
                            (e) => e.techniqueId == item.id,
                          ),
                      typedResults: items,
                    ),
                ];
              },
            );
          },
        ),
      );
}

typedef $$TechniquesTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $TechniquesTable,
      TechniqueRow,
      $$TechniquesTableFilterComposer,
      $$TechniquesTableOrderingComposer,
      $$TechniquesTableAnnotationComposer,
      $$TechniquesTableCreateCompanionBuilder,
      $$TechniquesTableUpdateCompanionBuilder,
      (TechniqueRow, $$TechniquesTableReferences),
      TechniqueRow,
      PrefetchHooks Function({bool techniqueMediaRefs})
    >;
typedef $$TechniqueMediaTableCreateCompanionBuilder =
    TechniqueMediaCompanion Function({
      required String id,
      required int createdAt,
      required int updatedAt,
      Value<int?> deletedAt,
      required int hlcWall,
      required int hlcCtr,
      required String hlcNode,
      required String hlcPacked,
      Value<int> schemaV,
      required String techniqueId,
      required String assetId,
      Value<String> kind,
      Value<String> label,
      Value<int> orderIndex,
      Value<int> rowid,
    });
typedef $$TechniqueMediaTableUpdateCompanionBuilder =
    TechniqueMediaCompanion Function({
      Value<String> id,
      Value<int> createdAt,
      Value<int> updatedAt,
      Value<int?> deletedAt,
      Value<int> hlcWall,
      Value<int> hlcCtr,
      Value<String> hlcNode,
      Value<String> hlcPacked,
      Value<int> schemaV,
      Value<String> techniqueId,
      Value<String> assetId,
      Value<String> kind,
      Value<String> label,
      Value<int> orderIndex,
      Value<int> rowid,
    });

final class $$TechniqueMediaTableReferences
    extends
        BaseReferences<_$AppDatabase, $TechniqueMediaTable, TechniqueMediaRow> {
  $$TechniqueMediaTableReferences(
    super.$_db,
    super.$_table,
    super.$_typedResult,
  );

  static $TechniquesTable _techniqueIdTable(_$AppDatabase db) => db.techniques
      .createAlias('technique_media__technique_id__techniques__id');

  $$TechniquesTableProcessedTableManager get techniqueId {
    final $_column = $_itemColumn<String>('technique_id')!;

    final manager = $$TechniquesTableTableManager(
      $_db,
      $_db.techniques,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_techniqueIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }

  static $AssetsTable _assetIdTable(_$AppDatabase db) =>
      db.assets.createAlias('technique_media__asset_id__assets__id');

  $$AssetsTableProcessedTableManager get assetId {
    final $_column = $_itemColumn<String>('asset_id')!;

    final manager = $$AssetsTableTableManager(
      $_db,
      $_db.assets,
    ).filter((f) => f.id.sqlEquals($_column));
    final item = $_typedResult.readTableOrNull(_assetIdTable($_db));
    if (item == null) return manager;
    return ProcessedTableManager(
      manager.$state.copyWith(prefetchedData: [item]),
    );
  }
}

class $$TechniqueMediaTableFilterComposer
    extends Composer<_$AppDatabase, $TechniqueMediaTable> {
  $$TechniqueMediaTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get kind => $composableBuilder(
    column: $table.kind,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get label => $composableBuilder(
    column: $table.label,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnFilters(column),
  );

  $$TechniquesTableFilterComposer get techniqueId {
    final $$TechniquesTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.techniqueId,
      referencedTable: $db.techniques,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniquesTableFilterComposer(
            $db: $db,
            $table: $db.techniques,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$AssetsTableFilterComposer get assetId {
    final $$AssetsTableFilterComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.assetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableFilterComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$TechniqueMediaTableOrderingComposer
    extends Composer<_$AppDatabase, $TechniqueMediaTable> {
  $$TechniqueMediaTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get updatedAt => $composableBuilder(
    column: $table.updatedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get deletedAt => $composableBuilder(
    column: $table.deletedAt,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcWall => $composableBuilder(
    column: $table.hlcWall,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get hlcCtr => $composableBuilder(
    column: $table.hlcCtr,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcNode => $composableBuilder(
    column: $table.hlcNode,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get schemaV => $composableBuilder(
    column: $table.schemaV,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get kind => $composableBuilder(
    column: $table.kind,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get label => $composableBuilder(
    column: $table.label,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => ColumnOrderings(column),
  );

  $$TechniquesTableOrderingComposer get techniqueId {
    final $$TechniquesTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.techniqueId,
      referencedTable: $db.techniques,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniquesTableOrderingComposer(
            $db: $db,
            $table: $db.techniques,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$AssetsTableOrderingComposer get assetId {
    final $$AssetsTableOrderingComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.assetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableOrderingComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$TechniqueMediaTableAnnotationComposer
    extends Composer<_$AppDatabase, $TechniqueMediaTable> {
  $$TechniqueMediaTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);

  GeneratedColumn<int> get updatedAt =>
      $composableBuilder(column: $table.updatedAt, builder: (column) => column);

  GeneratedColumn<int> get deletedAt =>
      $composableBuilder(column: $table.deletedAt, builder: (column) => column);

  GeneratedColumn<int> get hlcWall =>
      $composableBuilder(column: $table.hlcWall, builder: (column) => column);

  GeneratedColumn<int> get hlcCtr =>
      $composableBuilder(column: $table.hlcCtr, builder: (column) => column);

  GeneratedColumn<String> get hlcNode =>
      $composableBuilder(column: $table.hlcNode, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<int> get schemaV =>
      $composableBuilder(column: $table.schemaV, builder: (column) => column);

  GeneratedColumn<String> get kind =>
      $composableBuilder(column: $table.kind, builder: (column) => column);

  GeneratedColumn<String> get label =>
      $composableBuilder(column: $table.label, builder: (column) => column);

  GeneratedColumn<int> get orderIndex => $composableBuilder(
    column: $table.orderIndex,
    builder: (column) => column,
  );

  $$TechniquesTableAnnotationComposer get techniqueId {
    final $$TechniquesTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.techniqueId,
      referencedTable: $db.techniques,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$TechniquesTableAnnotationComposer(
            $db: $db,
            $table: $db.techniques,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }

  $$AssetsTableAnnotationComposer get assetId {
    final $$AssetsTableAnnotationComposer composer = $composerBuilder(
      composer: this,
      getCurrentColumn: (t) => t.assetId,
      referencedTable: $db.assets,
      getReferencedColumn: (t) => t.id,
      builder:
          (
            joinBuilder, {
            $addJoinBuilderToRootComposer,
            $removeJoinBuilderFromRootComposer,
          }) => $$AssetsTableAnnotationComposer(
            $db: $db,
            $table: $db.assets,
            $addJoinBuilderToRootComposer: $addJoinBuilderToRootComposer,
            joinBuilder: joinBuilder,
            $removeJoinBuilderFromRootComposer:
                $removeJoinBuilderFromRootComposer,
          ),
    );
    return composer;
  }
}

class $$TechniqueMediaTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $TechniqueMediaTable,
          TechniqueMediaRow,
          $$TechniqueMediaTableFilterComposer,
          $$TechniqueMediaTableOrderingComposer,
          $$TechniqueMediaTableAnnotationComposer,
          $$TechniqueMediaTableCreateCompanionBuilder,
          $$TechniqueMediaTableUpdateCompanionBuilder,
          (TechniqueMediaRow, $$TechniqueMediaTableReferences),
          TechniqueMediaRow,
          PrefetchHooks Function({bool techniqueId, bool assetId})
        > {
  $$TechniqueMediaTableTableManager(
    _$AppDatabase db,
    $TechniqueMediaTable table,
  ) : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$TechniqueMediaTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$TechniqueMediaTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$TechniqueMediaTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
                Value<int> updatedAt = const Value.absent(),
                Value<int?> deletedAt = const Value.absent(),
                Value<int> hlcWall = const Value.absent(),
                Value<int> hlcCtr = const Value.absent(),
                Value<String> hlcNode = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<int> schemaV = const Value.absent(),
                Value<String> techniqueId = const Value.absent(),
                Value<String> assetId = const Value.absent(),
                Value<String> kind = const Value.absent(),
                Value<String> label = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => TechniqueMediaCompanion(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                techniqueId: techniqueId,
                assetId: assetId,
                kind: kind,
                label: label,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required int createdAt,
                required int updatedAt,
                Value<int?> deletedAt = const Value.absent(),
                required int hlcWall,
                required int hlcCtr,
                required String hlcNode,
                required String hlcPacked,
                Value<int> schemaV = const Value.absent(),
                required String techniqueId,
                required String assetId,
                Value<String> kind = const Value.absent(),
                Value<String> label = const Value.absent(),
                Value<int> orderIndex = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => TechniqueMediaCompanion.insert(
                id: id,
                createdAt: createdAt,
                updatedAt: updatedAt,
                deletedAt: deletedAt,
                hlcWall: hlcWall,
                hlcCtr: hlcCtr,
                hlcNode: hlcNode,
                hlcPacked: hlcPacked,
                schemaV: schemaV,
                techniqueId: techniqueId,
                assetId: assetId,
                kind: kind,
                label: label,
                orderIndex: orderIndex,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map(
                (e) => (
                  e.readTable(table),
                  $$TechniqueMediaTableReferences(db, table, e),
                ),
              )
              .toList(),
          prefetchHooksCallback: ({techniqueId = false, assetId = false}) {
            return PrefetchHooks(
              db: db,
              explicitlyWatchedTables: [],
              addJoins:
                  <
                    T extends TableManagerState<
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic,
                      dynamic
                    >
                  >(state) {
                    if (techniqueId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.techniqueId,
                                referencedTable: $$TechniqueMediaTableReferences
                                    ._techniqueIdTable(db),
                                referencedColumn:
                                    $$TechniqueMediaTableReferences
                                        ._techniqueIdTable(db)
                                        .id,
                              )
                              as T;
                    }
                    if (assetId) {
                      state =
                          state.withJoin(
                                currentTable: table,
                                currentColumn: table.assetId,
                                referencedTable: $$TechniqueMediaTableReferences
                                    ._assetIdTable(db),
                                referencedColumn:
                                    $$TechniqueMediaTableReferences
                                        ._assetIdTable(db)
                                        .id,
                              )
                              as T;
                    }

                    return state;
                  },
              getPrefetchedDataCallback: (items) async {
                return [];
              },
            );
          },
        ),
      );
}

typedef $$TechniqueMediaTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $TechniqueMediaTable,
      TechniqueMediaRow,
      $$TechniqueMediaTableFilterComposer,
      $$TechniqueMediaTableOrderingComposer,
      $$TechniqueMediaTableAnnotationComposer,
      $$TechniqueMediaTableCreateCompanionBuilder,
      $$TechniqueMediaTableUpdateCompanionBuilder,
      (TechniqueMediaRow, $$TechniqueMediaTableReferences),
      TechniqueMediaRow,
      PrefetchHooks Function({bool techniqueId, bool assetId})
    >;
typedef $$OplogTableCreateCompanionBuilder =
    OplogCompanion Function({
      Value<int> seq,
      required String entityTable,
      required String entityId,
      required String op,
      required String hlcPacked,
      required String payloadHash,
      Value<bool> synced,
      Value<String?> dependsOnAssetId,
      required int createdAt,
    });
typedef $$OplogTableUpdateCompanionBuilder =
    OplogCompanion Function({
      Value<int> seq,
      Value<String> entityTable,
      Value<String> entityId,
      Value<String> op,
      Value<String> hlcPacked,
      Value<String> payloadHash,
      Value<bool> synced,
      Value<String?> dependsOnAssetId,
      Value<int> createdAt,
    });

class $$OplogTableFilterComposer extends Composer<_$AppDatabase, $OplogTable> {
  $$OplogTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<int> get seq => $composableBuilder(
    column: $table.seq,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get entityTable => $composableBuilder(
    column: $table.entityTable,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get entityId => $composableBuilder(
    column: $table.entityId,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get op => $composableBuilder(
    column: $table.op,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get payloadHash => $composableBuilder(
    column: $table.payloadHash,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<bool> get synced => $composableBuilder(
    column: $table.synced,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get dependsOnAssetId => $composableBuilder(
    column: $table.dependsOnAssetId,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnFilters(column),
  );
}

class $$OplogTableOrderingComposer
    extends Composer<_$AppDatabase, $OplogTable> {
  $$OplogTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<int> get seq => $composableBuilder(
    column: $table.seq,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get entityTable => $composableBuilder(
    column: $table.entityTable,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get entityId => $composableBuilder(
    column: $table.entityId,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get op => $composableBuilder(
    column: $table.op,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get hlcPacked => $composableBuilder(
    column: $table.hlcPacked,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get payloadHash => $composableBuilder(
    column: $table.payloadHash,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<bool> get synced => $composableBuilder(
    column: $table.synced,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get dependsOnAssetId => $composableBuilder(
    column: $table.dependsOnAssetId,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get createdAt => $composableBuilder(
    column: $table.createdAt,
    builder: (column) => ColumnOrderings(column),
  );
}

class $$OplogTableAnnotationComposer
    extends Composer<_$AppDatabase, $OplogTable> {
  $$OplogTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<int> get seq =>
      $composableBuilder(column: $table.seq, builder: (column) => column);

  GeneratedColumn<String> get entityTable => $composableBuilder(
    column: $table.entityTable,
    builder: (column) => column,
  );

  GeneratedColumn<String> get entityId =>
      $composableBuilder(column: $table.entityId, builder: (column) => column);

  GeneratedColumn<String> get op =>
      $composableBuilder(column: $table.op, builder: (column) => column);

  GeneratedColumn<String> get hlcPacked =>
      $composableBuilder(column: $table.hlcPacked, builder: (column) => column);

  GeneratedColumn<String> get payloadHash => $composableBuilder(
    column: $table.payloadHash,
    builder: (column) => column,
  );

  GeneratedColumn<bool> get synced =>
      $composableBuilder(column: $table.synced, builder: (column) => column);

  GeneratedColumn<String> get dependsOnAssetId => $composableBuilder(
    column: $table.dependsOnAssetId,
    builder: (column) => column,
  );

  GeneratedColumn<int> get createdAt =>
      $composableBuilder(column: $table.createdAt, builder: (column) => column);
}

class $$OplogTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $OplogTable,
          OplogRow,
          $$OplogTableFilterComposer,
          $$OplogTableOrderingComposer,
          $$OplogTableAnnotationComposer,
          $$OplogTableCreateCompanionBuilder,
          $$OplogTableUpdateCompanionBuilder,
          (OplogRow, BaseReferences<_$AppDatabase, $OplogTable, OplogRow>),
          OplogRow,
          PrefetchHooks Function()
        > {
  $$OplogTableTableManager(_$AppDatabase db, $OplogTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$OplogTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$OplogTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$OplogTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<int> seq = const Value.absent(),
                Value<String> entityTable = const Value.absent(),
                Value<String> entityId = const Value.absent(),
                Value<String> op = const Value.absent(),
                Value<String> hlcPacked = const Value.absent(),
                Value<String> payloadHash = const Value.absent(),
                Value<bool> synced = const Value.absent(),
                Value<String?> dependsOnAssetId = const Value.absent(),
                Value<int> createdAt = const Value.absent(),
              }) => OplogCompanion(
                seq: seq,
                entityTable: entityTable,
                entityId: entityId,
                op: op,
                hlcPacked: hlcPacked,
                payloadHash: payloadHash,
                synced: synced,
                dependsOnAssetId: dependsOnAssetId,
                createdAt: createdAt,
              ),
          createCompanionCallback:
              ({
                Value<int> seq = const Value.absent(),
                required String entityTable,
                required String entityId,
                required String op,
                required String hlcPacked,
                required String payloadHash,
                Value<bool> synced = const Value.absent(),
                Value<String?> dependsOnAssetId = const Value.absent(),
                required int createdAt,
              }) => OplogCompanion.insert(
                seq: seq,
                entityTable: entityTable,
                entityId: entityId,
                op: op,
                hlcPacked: hlcPacked,
                payloadHash: payloadHash,
                synced: synced,
                dependsOnAssetId: dependsOnAssetId,
                createdAt: createdAt,
              ),
          withReferenceMapper: (p0) => p0
              .map((e) => (e.readTable(table), BaseReferences(db, table, e)))
              .toList(),
          prefetchHooksCallback: null,
        ),
      );
}

typedef $$OplogTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $OplogTable,
      OplogRow,
      $$OplogTableFilterComposer,
      $$OplogTableOrderingComposer,
      $$OplogTableAnnotationComposer,
      $$OplogTableCreateCompanionBuilder,
      $$OplogTableUpdateCompanionBuilder,
      (OplogRow, BaseReferences<_$AppDatabase, $OplogTable, OplogRow>),
      OplogRow,
      PrefetchHooks Function()
    >;
typedef $$RecordingsInflightTableCreateCompanionBuilder =
    RecordingsInflightCompanion Function({
      required String id,
      required String practiceId,
      required int pid,
      required int startedAt,
      Value<int> rowid,
    });
typedef $$RecordingsInflightTableUpdateCompanionBuilder =
    RecordingsInflightCompanion Function({
      Value<String> id,
      Value<String> practiceId,
      Value<int> pid,
      Value<int> startedAt,
      Value<int> rowid,
    });

class $$RecordingsInflightTableFilterComposer
    extends Composer<_$AppDatabase, $RecordingsInflightTable> {
  $$RecordingsInflightTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<String> get practiceId => $composableBuilder(
    column: $table.practiceId,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get pid => $composableBuilder(
    column: $table.pid,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get startedAt => $composableBuilder(
    column: $table.startedAt,
    builder: (column) => ColumnFilters(column),
  );
}

class $$RecordingsInflightTableOrderingComposer
    extends Composer<_$AppDatabase, $RecordingsInflightTable> {
  $$RecordingsInflightTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get id => $composableBuilder(
    column: $table.id,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<String> get practiceId => $composableBuilder(
    column: $table.practiceId,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get pid => $composableBuilder(
    column: $table.pid,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get startedAt => $composableBuilder(
    column: $table.startedAt,
    builder: (column) => ColumnOrderings(column),
  );
}

class $$RecordingsInflightTableAnnotationComposer
    extends Composer<_$AppDatabase, $RecordingsInflightTable> {
  $$RecordingsInflightTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get id =>
      $composableBuilder(column: $table.id, builder: (column) => column);

  GeneratedColumn<String> get practiceId => $composableBuilder(
    column: $table.practiceId,
    builder: (column) => column,
  );

  GeneratedColumn<int> get pid =>
      $composableBuilder(column: $table.pid, builder: (column) => column);

  GeneratedColumn<int> get startedAt =>
      $composableBuilder(column: $table.startedAt, builder: (column) => column);
}

class $$RecordingsInflightTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $RecordingsInflightTable,
          RecordingInflightRow,
          $$RecordingsInflightTableFilterComposer,
          $$RecordingsInflightTableOrderingComposer,
          $$RecordingsInflightTableAnnotationComposer,
          $$RecordingsInflightTableCreateCompanionBuilder,
          $$RecordingsInflightTableUpdateCompanionBuilder,
          (
            RecordingInflightRow,
            BaseReferences<
              _$AppDatabase,
              $RecordingsInflightTable,
              RecordingInflightRow
            >,
          ),
          RecordingInflightRow,
          PrefetchHooks Function()
        > {
  $$RecordingsInflightTableTableManager(
    _$AppDatabase db,
    $RecordingsInflightTable table,
  ) : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$RecordingsInflightTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$RecordingsInflightTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$RecordingsInflightTableAnnotationComposer(
                $db: db,
                $table: table,
              ),
          updateCompanionCallback:
              ({
                Value<String> id = const Value.absent(),
                Value<String> practiceId = const Value.absent(),
                Value<int> pid = const Value.absent(),
                Value<int> startedAt = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => RecordingsInflightCompanion(
                id: id,
                practiceId: practiceId,
                pid: pid,
                startedAt: startedAt,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String id,
                required String practiceId,
                required int pid,
                required int startedAt,
                Value<int> rowid = const Value.absent(),
              }) => RecordingsInflightCompanion.insert(
                id: id,
                practiceId: practiceId,
                pid: pid,
                startedAt: startedAt,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map((e) => (e.readTable(table), BaseReferences(db, table, e)))
              .toList(),
          prefetchHooksCallback: null,
        ),
      );
}

typedef $$RecordingsInflightTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $RecordingsInflightTable,
      RecordingInflightRow,
      $$RecordingsInflightTableFilterComposer,
      $$RecordingsInflightTableOrderingComposer,
      $$RecordingsInflightTableAnnotationComposer,
      $$RecordingsInflightTableCreateCompanionBuilder,
      $$RecordingsInflightTableUpdateCompanionBuilder,
      (
        RecordingInflightRow,
        BaseReferences<
          _$AppDatabase,
          $RecordingsInflightTable,
          RecordingInflightRow
        >,
      ),
      RecordingInflightRow,
      PrefetchHooks Function()
    >;
typedef $$WaveformCacheTableCreateCompanionBuilder =
    WaveformCacheCompanion Function({
      required String contentHash,
      required int algoVersion,
      required Uint8List peaks,
      Value<int> rowid,
    });
typedef $$WaveformCacheTableUpdateCompanionBuilder =
    WaveformCacheCompanion Function({
      Value<String> contentHash,
      Value<int> algoVersion,
      Value<Uint8List> peaks,
      Value<int> rowid,
    });

class $$WaveformCacheTableFilterComposer
    extends Composer<_$AppDatabase, $WaveformCacheTable> {
  $$WaveformCacheTableFilterComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnFilters<String> get contentHash => $composableBuilder(
    column: $table.contentHash,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<int> get algoVersion => $composableBuilder(
    column: $table.algoVersion,
    builder: (column) => ColumnFilters(column),
  );

  ColumnFilters<Uint8List> get peaks => $composableBuilder(
    column: $table.peaks,
    builder: (column) => ColumnFilters(column),
  );
}

class $$WaveformCacheTableOrderingComposer
    extends Composer<_$AppDatabase, $WaveformCacheTable> {
  $$WaveformCacheTableOrderingComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  ColumnOrderings<String> get contentHash => $composableBuilder(
    column: $table.contentHash,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<int> get algoVersion => $composableBuilder(
    column: $table.algoVersion,
    builder: (column) => ColumnOrderings(column),
  );

  ColumnOrderings<Uint8List> get peaks => $composableBuilder(
    column: $table.peaks,
    builder: (column) => ColumnOrderings(column),
  );
}

class $$WaveformCacheTableAnnotationComposer
    extends Composer<_$AppDatabase, $WaveformCacheTable> {
  $$WaveformCacheTableAnnotationComposer({
    required super.$db,
    required super.$table,
    super.joinBuilder,
    super.$addJoinBuilderToRootComposer,
    super.$removeJoinBuilderFromRootComposer,
  });
  GeneratedColumn<String> get contentHash => $composableBuilder(
    column: $table.contentHash,
    builder: (column) => column,
  );

  GeneratedColumn<int> get algoVersion => $composableBuilder(
    column: $table.algoVersion,
    builder: (column) => column,
  );

  GeneratedColumn<Uint8List> get peaks =>
      $composableBuilder(column: $table.peaks, builder: (column) => column);
}

class $$WaveformCacheTableTableManager
    extends
        RootTableManager<
          _$AppDatabase,
          $WaveformCacheTable,
          WaveformCacheRow,
          $$WaveformCacheTableFilterComposer,
          $$WaveformCacheTableOrderingComposer,
          $$WaveformCacheTableAnnotationComposer,
          $$WaveformCacheTableCreateCompanionBuilder,
          $$WaveformCacheTableUpdateCompanionBuilder,
          (
            WaveformCacheRow,
            BaseReferences<
              _$AppDatabase,
              $WaveformCacheTable,
              WaveformCacheRow
            >,
          ),
          WaveformCacheRow,
          PrefetchHooks Function()
        > {
  $$WaveformCacheTableTableManager(_$AppDatabase db, $WaveformCacheTable table)
    : super(
        TableManagerState(
          db: db,
          table: table,
          createFilteringComposer: () =>
              $$WaveformCacheTableFilterComposer($db: db, $table: table),
          createOrderingComposer: () =>
              $$WaveformCacheTableOrderingComposer($db: db, $table: table),
          createComputedFieldComposer: () =>
              $$WaveformCacheTableAnnotationComposer($db: db, $table: table),
          updateCompanionCallback:
              ({
                Value<String> contentHash = const Value.absent(),
                Value<int> algoVersion = const Value.absent(),
                Value<Uint8List> peaks = const Value.absent(),
                Value<int> rowid = const Value.absent(),
              }) => WaveformCacheCompanion(
                contentHash: contentHash,
                algoVersion: algoVersion,
                peaks: peaks,
                rowid: rowid,
              ),
          createCompanionCallback:
              ({
                required String contentHash,
                required int algoVersion,
                required Uint8List peaks,
                Value<int> rowid = const Value.absent(),
              }) => WaveformCacheCompanion.insert(
                contentHash: contentHash,
                algoVersion: algoVersion,
                peaks: peaks,
                rowid: rowid,
              ),
          withReferenceMapper: (p0) => p0
              .map((e) => (e.readTable(table), BaseReferences(db, table, e)))
              .toList(),
          prefetchHooksCallback: null,
        ),
      );
}

typedef $$WaveformCacheTableProcessedTableManager =
    ProcessedTableManager<
      _$AppDatabase,
      $WaveformCacheTable,
      WaveformCacheRow,
      $$WaveformCacheTableFilterComposer,
      $$WaveformCacheTableOrderingComposer,
      $$WaveformCacheTableAnnotationComposer,
      $$WaveformCacheTableCreateCompanionBuilder,
      $$WaveformCacheTableUpdateCompanionBuilder,
      (
        WaveformCacheRow,
        BaseReferences<_$AppDatabase, $WaveformCacheTable, WaveformCacheRow>,
      ),
      WaveformCacheRow,
      PrefetchHooks Function()
    >;

class $AppDatabaseManager {
  final _$AppDatabase _db;
  $AppDatabaseManager(this._db);
  $$AssetsTableTableManager get assets =>
      $$AssetsTableTableManager(_db, _db.assets);
  $$PracticesTableTableManager get practices =>
      $$PracticesTableTableManager(_db, _db.practices);
  $$InterpretationsTableTableManager get interpretations =>
      $$InterpretationsTableTableManager(_db, _db.interpretations);
  $$FxBusesTableTableManager get fxBuses =>
      $$FxBusesTableTableManager(_db, _db.fxBuses);
  $$TracksTableTableManager get tracks =>
      $$TracksTableTableManager(_db, _db.tracks);
  $$ClipsTableTableManager get clips =>
      $$ClipsTableTableManager(_db, _db.clips);
  $$PracticeNotesTableTableManager get practiceNotes =>
      $$PracticeNotesTableTableManager(_db, _db.practiceNotes);
  $$TechniqueMarkersTableTableManager get techniqueMarkers =>
      $$TechniqueMarkersTableTableManager(_db, _db.techniqueMarkers);
  $$TechniquesTableTableManager get techniques =>
      $$TechniquesTableTableManager(_db, _db.techniques);
  $$TechniqueMediaTableTableManager get techniqueMedia =>
      $$TechniqueMediaTableTableManager(_db, _db.techniqueMedia);
  $$OplogTableTableManager get oplog =>
      $$OplogTableTableManager(_db, _db.oplog);
  $$RecordingsInflightTableTableManager get recordingsInflight =>
      $$RecordingsInflightTableTableManager(_db, _db.recordingsInflight);
  $$WaveformCacheTableTableManager get waveformCache =>
      $$WaveformCacheTableTableManager(_db, _db.waveformCache);
}
