use std::result::Result as StdResult;
use thiserror::Error as ThisError;

/// Unified S7 result type
pub type Result<T> = StdResult<T, Error>;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("connect timeout")]
    ErrConnectTimeout,

    #[error("request timeout")]
    ErrRequestTimeout,

    #[error("invalid frame")]
    ErrInvalidFrame,

    #[error("unexpected PDU or function")]
    ErrUnexpectedPdu,

    #[error("invalid address: {0}")]
    ErrInvalidAddress(String),

    #[error("invalid parameter")]
    ErrInvalidParam,

    #[error("can not use closed connection")]
    ErrUseClosedConnection,

    #[error("session is not active")]
    ErrNotActive,

    /// Too many concurrent in-flight requests, exceeds negotiated amq_callee or local limit
    #[error("too many in-flight requests")]
    ErrTooManyInFlight,

    #[error("invalid configuration for: {0}")]
    InvalidConfiguration(&'static str),

    /// Decode error for wire-format parsing failures that are not protocol violations but malformed/invalid bytes
    #[error("decode error: {context}")]
    Decode { context: &'static str },

    /// Encode error for wire-format serialization failures
    #[error("encode error: {context}")]
    Encode { context: &'static str },

    /// Protocol contract violated (e.g., reserved/invalid field values)
    #[error("protocol violation: {context}")]
    ProtocolViolation { context: &'static str },

    /// Input does not have enough bytes to complete the operation
    #[error("insufficient data: needed {needed} bytes, available {available} bytes")]
    InsufficientData { needed: usize, available: usize },

    /// Feature or PDU type is recognized but not supported by this implementation
    #[error("unsupported feature: {feature}")]
    UnsupportedFeature { feature: &'static str },

    /// S7 Ack/AckData header-level error with specific `ErrorCode`.
    ///
    /// This is returned when a PLC responds with an error code in the
    /// header's Ack/AckData section. Callers can match on the contained
    /// `ErrorCode` to make fine-grained decisions.
    #[allow(clippy::enum_variant_names)]
    #[error("S7 error: {code:?}")]
    S7Error { code: ErrorCode },

    /// TSAP-specific: Rack value is out of allowed range (0..=15)
    #[error("Rack value {0} is out of range (0..=15)")]
    InvalidRack(u8),

    /// TSAP-specific: Slot value is out of allowed range (0..=15)
    #[error("Slot value {0} is out of range (0..=15)")]
    InvalidSlot(u8),
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// 成功
    Success = 0x0000,
    /// 块号无效
    InvalidBlockNumber = 0x0110,
    /// 请求长度无效
    InvalidRequestLength = 0x0111,
    /// 参数无效
    InvalidParams = 0x0112,
    /// 块类型无效
    InvalidBlockType = 0x0113,
    /// 未找到块
    BlockNotFound = 0x0114,
    /// 块已存在
    BlockAlreadyExists = 0x0115,
    /// 块被写保护
    BlockWriteProtected = 0x0116,
    /// 块更新过大
    BlockUpdateTooLarge = 0x0117,
    /// 块编号无效
    BlockNumberInvalid = 0x0118,
    /// 密码无效
    InvalidPassword = 0x0119,
    /// PG 资源错误
    PGResourceError = 0x011A,
    /// PLC 资源错误
    PLCResourceError = 0x011B,
    /// 协议错误
    ProtocolError = 0x011C,
    /// 块过多
    TooManyBlock = 0x011D,
    /// 会话已过期
    SessionExpired = 0x011E,
    /// 结果缓冲区太小
    ResultBufferTooSmall = 0x011F,
    /// 块列表结束
    BlockEndList = 0x0120,
    /// 可用内存不足
    InsufficientAvailableMemory = 0x0140,
    /// 作业未处理
    JobNotProcessed = 0x0141,
    /// 当块处于当前状态时无法执行请求的服务
    ServiceNotAllowedInCurrentState = 0x8001,
    /// S7协议错误：传输块时发生错误
    BlockTransferProtocolError = 0x8003,
    /// 应用程序错误：远程模块未知的服务
    UnknownServiceOnRemoteModule = 0x8100,
    /// 服务未实现或出现帧错误
    ServiceNotImplementedOrFrameError = 0x8104,
    /// 对象类型不一致
    ObjectTypeMismatch = 0x8204,
    /// 复制的块已存在且未链接
    CopiedBlockExistsUnlinked = 0x8205,
    /// 内存/工作内存不足或介质不可访问
    MemoryOrStorageUnavailable = 0x8301,
    /// 资源不足或处理器资源不可用
    ResourcesInsufficientOrCpuBusy = 0x8302,
    /// 无法继续并行上传（资源瓶颈）
    ParallelUploadNotPossible = 0x8304,
    /// 功能不可用
    FunctionUnavailable = 0x8305,
    /// 工作内存不足
    WorkMemoryInsufficient = 0x8306,
    /// 保持性工作内存不足
    RetentiveMemoryInsufficient = 0x8307,
    /// S7协议错误：无效的服务序列
    InvalidServiceSequence = 0x8401,
    /// 受对象状态影响，服务无法执行
    ServiceBlockedByObjectState = 0x8402,
    /// S7协议：无法执行该功能
    FunctionCannotBeExecuted = 0x8404,
    /// 远程块处于禁用状态
    RemoteBlockDisabled = 0x8405,
    /// S7协议错误：帧错误
    FrameError = 0x8500,
    /// 模块报警：服务过早取消
    ServiceAbortedEarly = 0x8503,
    /// 寻址通信伙伴对象出错（如区域长度错误）
    ObjectAddressingError = 0x8701,
    /// 模块不支持所请求的服务
    ServiceNotSupportedByModule = 0x8702,
    /// 拒绝访问对象
    ObjectAccessDenied = 0x8703,
    /// 访问错误：对象损坏
    ObjectCorrupted = 0x8704,
    /// 协议错误：非法的作业号
    IllegalJobNumber = 0xD001,
    /// 参数错误：非法的作业变体
    IllegalJobVariant = 0xD002,
    /// 参数错误：模块不支持调试功能
    DebugNotSupported = 0xD003,
    /// 参数错误：作业状态非法
    IllegalJobState = 0xD004,
    /// 参数错误：作业终止非法
    IllegalJobAbort = 0xD005,
    /// 参数错误：非法链路断开ID
    IllegalDisconnectId = 0xD006,
    /// 参数错误：缓冲区元素数量非法
    IllegalBufferElementCount = 0xD007,
    /// 参数错误：扫描速率非法
    IllegalScanRate = 0xD008,
    /// 参数错误：执行次数非法
    IllegalExecutionCount = 0xD009,
    /// 参数错误：非法触发事件
    IllegalTriggerEvent = 0xD00A,
    /// 参数错误：非法触发条件
    IllegalTriggerCondition = 0xD00B,
    /// 调用路径错误：块不存在
    BlockNotExistInPath = 0xD011,
    /// 参数错误：块内地址错误
    AddressErrorInBlock = 0xD012,
    /// 参数错误：正在删除/覆盖块
    DeletingOrOverwritingBlock = 0xD014,
    /// 参数错误：标签地址非法
    IllegalTagAddress = 0xD015,
    /// 参数错误：用户程序错误，无法测试作业
    TestJobNotPossibleUserProgramError = 0xD016,
    /// 参数错误：非法触发号
    IllegalTriggerNumber = 0xD017,
    /// 参数错误：路径无效
    InvalidPath = 0xD025,
    /// 参数错误：非法访问类型
    IllegalAccessType = 0xD026,
    /// 参数错误：数据块数量不允许
    TooManyDataBlocks = 0xD027,
    /// 内部协议错误
    InternalProtocolError = 0xD031,
    /// 参数错误：结果缓冲区长度错误
    ResultBufferLengthError = 0xD032,
    /// 协议错误：作业长度错误
    JobLengthError = 0xD033,
    /// 编码错误：参数部分错误
    ParamSectionEncodingError = 0xD03F,
    /// 数据错误：非法状态列表ID
    IllegalStateListId = 0xD041,
    /// 数据错误：标签地址非法
    IllegalTagAddressData = 0xD042,
    /// 数据错误：找不到引用的作业
    ReferencedJobMissing = 0xD043,
    /// 数据错误：标签值非法
    IllegalTagValue = 0xD044,
    /// 数据错误：HOLD中不允许退出ODIS控制
    ODISExitNotAllowedInHold = 0xD045,
    /// 数据错误：运行时测量期间非法阶段
    IllegalMeasurementPhase = 0xD046,
    /// 数据错误：“读取作业列表”层级非法
    IllegalHierarchyInReadJobList = 0xD047,
    /// 数据错误：“删除作业”删除ID非法
    IllegalDeleteId = 0xD048,
    /// “替换作业”替换ID无效
    InvalidReplaceId = 0xD049,
    /// 执行“程序状态”时出错
    ProgramStatusError = 0xD04A,
    /// 编码错误：数据部分错误
    DataSectionEncodingError = 0xD05F,
    /// 资源错误：没有作业的内存空间
    NoMemoryForJob = 0xD061,
    /// 资源错误：作业列表已满
    JobListFull = 0xD062,
    /// 资源错误：触发事件被占用
    TriggerEventOccupied = 0xD063,
    /// 资源错误：结果缓冲区元素内存不足
    NoMemoryForResultElement = 0xD064,
    /// 资源错误：多个结果缓冲区元素内存不足
    NoMemoryForResultElements = 0xD065,
    /// 资源错误：用于测量的计时器被占用
    TimerOccupiedByAnotherJob = 0xD066,
    /// 资源错误：“修改标记”作业过多
    TooManyModifyJobs = 0xD067,
    /// 当前模式下不允许的功能
    FunctionNotAllowedInCurrentMode = 0xD081,
    /// 模式错误：无法退出HOLD模式
    CannotExitHoldMode = 0xD082,
    /// 当前保护级别不允许的功能
    FunctionNotAllowedAtProtectionLevel = 0xD0A1,
    /// 正在运行的函数会修改内存，暂不可运行
    BusyDueToMemoryModifyingFunction = 0xD0A2,
    /// I/O上活动的“修改标记”作业过多
    TooManyModifyJobsOnIO = 0xD0A3,
    /// 强制已建立
    ForcingAlreadyActive = 0xD0A4,
    /// 找不到引用的作业
    ReferencedJobNotFound = 0xD0A5,
    /// 无法禁用/启用作业
    CannotDisableEnableJob = 0xD0A6,
    /// 无法删除作业（可能正在读取）
    CannotDeleteJob = 0xD0A7,
    /// 无法替换作业（可能正在读/删）
    CannotReplaceJob = 0xD0A8,
    /// 无法读取作业（可能正在删除）
    CannotReadJob = 0xD0A9,
    /// 处理操作超出时间限制
    ProcessingTimeout = 0xD0AA,
    /// 进程操作中的作业参数无效
    InvalidProcessJobParams = 0xD0AB,
    /// 进程操作中的作业数据无效
    InvalidProcessJobData = 0xD0AC,
    /// 已设置操作模式
    OperationModeSet = 0xD0AD,
    /// 作业绑定于另一连接
    JobBoundToAnotherConnection = 0xD0AE,
    /// 访问标签时检测到错误
    TagAccessErrorDetected = 0xD0C1,
    /// 切换到STOP/HOLD模式
    SwitchedToStopOrHold = 0xD0C2,
    /// 标签访问错误，模式改为STOP/HOLD
    TagAccessErrorThenStopOrHold = 0xD0C3,
    /// 运行时测量超时
    RuntimeMeasurementTimeout = 0xD0C4,
    /// 块堆栈显示不一致（块被删/重载）
    BlockStackInconsistent = 0xD0C5,
    /// 引用作业被删导致本作业被删
    JobDeletedDueToReferencedJobDeleted = 0xD0C6,
    /// 退出STOP模式后作业被自动删除
    JobAutoDeletedDueToStopExit = 0xD0C7,
    /// 测试作业与运行程序不一致导致中止
    BlockStatusAbortedDueToInconsistency = 0xD0C8,
    /// 通过复位OB90退出状态区域
    ExitStateAreaByResetOB90 = 0xD0C9,
    /// 复位OB90且读取标签错误后退出状态范围
    ExitStateAreaByResetOB90AndReadError = 0xD0CA,
    /// 外设输出的输出禁用再次激活
    PeripheralOutputDisableReactivated = 0xD0CB,
    /// 调试功能数据量受时间限制
    DebugDataVolumeTimeLimited = 0xD0CC,
    /// 块名称语法错误
    SyntaxErrorInBlockName = 0xD201,
    /// 函数参数语法错误
    SyntaxErrorInFunctionParams = 0xD202,
    /// RAM中已存在链接块，无法条件复制
    LinkedBlockExistsInRAM = 0xD205,
    /// EPROM中已存在链接块，无法条件复制
    LinkedBlockExistsInEPROM = 0xD206,
    /// 超出模块最大未链接块数
    MaxUnlinkedBlocksExceeded = 0xD208,
    /// 至少有一个给定块在模块上找不到
    GivenBlockNotFoundOnModule = 0xD209,
    /// 超出单作业可链接的最大块数
    MaxBlocksLinkedPerJobExceeded = 0xD20A,
    /// 超出单作业可删除的最大块数
    MaxBlocksDeletablePerJobExceeded = 0xD20B,
    /// OB无法复制：关联优先级不存在
    OBPriorityMissing = 0xD20C,
    /// SDB无法解释（未知数等）
    SDBNotInterpretable = 0xD20D,
    /// 没有（更多）可用块
    NoMoreBlocksAvailable = 0xD20E,
    /// 超出模块特定的最大块大小
    ModuleSpecificMaxBlockSizeExceeded = 0xD20F,
    /// 块号无效
    InvalidBlockNumberD210 = 0xD210,
    /// 标头属性不正确（与运行时相关）
    HeaderAttributesIncorrect = 0xD212,
    /// SDB过多（受模块限制）
    TooManySDBs = 0xD213,
    /// 无效的用户程序，需重置模块
    InvalidUserProgramResetModule = 0xD216,
    /// 不允许的保护级别设置
    ProtectionLevelNotAllowed = 0xD217,
    /// 属性不正确（主动/被动）
    IncorrectAttributeActivePassive = 0xD218,
    /// 块长度不正确
    IncorrectBlockLength = 0xD219,
    /// 本地数据长度不正确或写保护错误
    IncorrectLocalDataLengthOrWriteProtect = 0xD21A,
    /// 模块无法压缩或压缩中断
    ModuleCannotCompressOrCompressionInterrupted = 0xD21B,
    /// 传输的动态项目数据量非法
    IllegalDynamicProjectDataSize = 0xD21D,
    /// 无法为模块分配参数（系统数据无法链接）
    CannotAssignParametersToModule = 0xD21E,
    /// 编程语言无效（受模块限制）
    InvalidProgrammingLanguage = 0xD220,
    /// 连接或路由的系统数据无效
    InvalidConnectionOrRoutingSystemData = 0xD221,
    /// 全局数据定义的系统数据包含无效参数
    InvalidGlobalDataSystemParams = 0xD222,
    /// 通信FB实例DB错误或超出最大背景DB数
    CommFBInstanceDbErrorOrTooManyBackgroundDb = 0xD223,
    /// SCAN系统数据块参数无效
    ScanSDBInvalidParams = 0xD224,
    /// DP系统数据块参数无效
    DpSDBInvalidParams = 0xD225,
    /// 块中发生结构错误
    StructuralErrorInBlock = 0xD226,
    /// 块中发生结构错误（再次）
    StructuralErrorInBlock2 = 0xD230,
    /// 已加载的OB无法复制：关联优先级不存在
    OBPriorityMissingOnCopy = 0xD231,
    /// 加载块的块编号非法
    IllegalBlockNumberInLoadedBlock = 0xD232,
    /// 块在指定介质或作业中存在两次
    BlockExistsTwice = 0xD234,
    /// 块校验和不正确
    BlockChecksumIncorrect = 0xD235,
    /// 块不包含校验和
    BlockNoChecksum = 0xD236,
    /// 目标CPU已有相同时间戳的块
    BlockAlreadyLoadedSameTimestamp = 0xD237,
    /// 指定的块中至少有一个不是DB
    SpecifiedBlockNotDB = 0xD238,
    /// 指定的DB在装载存储器中不可用作链接变量
    SpecifiedDBNotAvailableAsLinkVar = 0xD239,
    /// 指定的DB与复制/链接的变体差异过大
    SpecifiedDBDiffersTooMuch = 0xD23A,
    /// 违反协调规则
    CoordinationRulesViolated = 0xD240,
    /// 当前保护级别不允许该功能
    FunctionNotAllowedAtCurrentProtectionLevel = 0xD241,
    /// 处理F块时的保护冲突
    ProtectionConflictInFBlock = 0xD242,
    /// 更新与模块ID或版本不匹配
    UpdateAndModuleIdOrVersionMismatch = 0xD250,
    /// 操作系统组件序列不正确
    OSComponentsSequenceIncorrect = 0xD251,
    /// 校验和错误
    ChecksumError = 0xD252,
    /// 无可执行加载程序（仅可用存储卡更新）
    NoExecutableLoaderAvailable = 0xD253,
    /// 操作系统中的存储错误
    StorageErrorInOS = 0xD254,
    /// 在S7-300 CPU中编译块时出错
    CompileErrorOnS7300 = 0xD280,
    /// 块上另一个块功能或触发器处于活动
    AnotherBlockFunctionOrTriggerActive = 0xD2A1,
    /// 块触发器处于活动，先完成调试功能
    TriggerActiveFinishDebugFirst = 0xD2A2,
    /// 块未激活/被占用/标记为删除
    BlockNotActivatedOrBusyOrMarkedForDelete = 0xD2A3,
    /// 该块已被另一块功能处理
    BlockProcessedByAnotherFunction = 0xD2A4,
    /// 无法同时保存并修改用户程序
    CannotSaveAndChangeProgramSimultaneously = 0xD2A6,
    /// 块为未链接或未处理
    BlockUnlinkedOrNotProcessed = 0xD2A7,
    /// 激活的调试功能阻止分配CPU参数
    ActiveDebugPreventsCpuParameterAssignment = 0xD2A8,
    /// 正在为CPU分配新参数
    AssigningNewParamsToCpu = 0xD2A9,
    /// 正在为模块分配新参数
    AssigningNewParamsToModule = 0xD2AA,
    /// 正在更改动态配置限制
    ChangingDynamicConfigLimits = 0xD2AB,
    /// SFC12激活/取消激活暂时阻止R-KiR
    SFC12ActivationBlocksRKiR = 0xD2AC,
    /// RUN（CiR）中配置时发生错误
    ErrorConfiguringInRunCiR = 0xD2B0,
    /// 超出最大工艺对象数
    MaxProcessObjectsExceeded = 0xD2C0,
    /// 模块上已存在相同的技术数据块
    SameTechnologyDbExists = 0xD2C1,
    /// 无法下载用户程序或硬件配置
    CannotDownloadUserProgramOrHardwareConfig = 0xD2C2,
    /// 信息功能不可用
    InfoFunctionNotAvailable = 0xD401,
    /// 信息功能不可用
    InfoFunctionNotAvailable2 = 0xD402,
    /// 服务已登录/注销（诊断/PMC）
    ServiceLoggedInOut = 0xD403,
    /// 达到最大节点数，无需再登录诊断/PMC
    MaxNodesReached = 0xD404,
    /// 不支持服务或函数参数语法错误
    ServiceNotSupportedOrSyntaxErrorInParams = 0xD405,
    /// 所需信息当前不可用
    RequiredInfoCurrentlyUnavailable = 0xD406,
    /// 发生诊断错误
    DiagnosticErrorOccurred = 0xD407,
    /// 更新已中止
    UpdateAborted = 0xD408,
    /// DP总线错误
    DPBusError = 0xD409,
    /// 函数参数中的语法错误
    SyntaxErrorInFunctionParamsD601 = 0xD601,
    /// 输入的密码不正确
    IncorrectPassword = 0xD602,
    /// 连接已合法化
    ConnectionLegalized = 0xD603,
    /// 连接已启用
    ConnectionEnabled = 0xD604,
    /// 无法合法化：密码不存在
    LegalizationNotPossiblePasswordMissing = 0xD605,
    /// 至少一个标记地址无效
    AtLeastOneTagAddressInvalid = 0xD801,
    /// 指定的作业不存在
    SpecifiedJobDoesNotExist = 0xD802,
    /// 非法的工作状态
    IllegalOperatingState = 0xD803,
    /// 非法循环时间（时基或倍数非法）
    IllegalCycleTime = 0xD804,
    /// 不能再设置循环读取作业
    NoMoreCyclicReadJobs = 0xD805,
    /// 引用的作业处于不允许执行的状态
    ReferencedJobInWrongState = 0xD806,
    /// 因过载中止（读取周期超过设定周期）
    FunctionAbortedDueToOverload = 0xD807,
    /// 日期和/或时间无效
    InvalidDateOrTime = 0xDC01,
    /// CPU已经是主设备
    CpuAlreadyMaster = 0xE201,
    /// 由于闪存模块中的用户程序不同，无法进行连接和更新
    CannotConnectUpdateDueToDifferentUserProgram = 0xE202,
    /// 由于固件不同，无法连接和更新
    CannotConnectUpdateDueToDifferentFirmware = 0xE203,
    /// 由于内存配置不同，无法连接和更新
    CannotConnectUpdateDueToDifferentMemoryConfig = 0xE204,
    /// 由于同步错误导致连接/更新中止
    CannotConnectUpdateDueToSynchronization = 0xE205,
    /// 由于协调违规而拒绝连接/更新
    CannotConnectUpdateDueToCoordinationViolation = 0xE206,
    /// S7协议错误：ID2错误; 工作中只允许00H
    ErrorAtId2Only00HPermittedInJob = 0xEF01,
    /// S7协议错误：ID2错误; 资源集不存在
    ErrorAtId2SetOfResourcesDoesNotExist = 0xEF02,
}

impl TryFrom<u16> for ErrorCode {
    type Error = ();

    fn try_from(value: u16) -> StdResult<Self, Self::Error> {
        match value {
            0x0000 => Ok(Self::Success),
            0x0110 => Ok(Self::InvalidBlockNumber),
            0x0111 => Ok(Self::InvalidRequestLength),
            0x0112 => Ok(Self::InvalidParams),
            0x0113 => Ok(Self::InvalidBlockType),
            0x0114 => Ok(Self::BlockNotFound),
            0x0115 => Ok(Self::BlockAlreadyExists),
            0x0116 => Ok(Self::BlockWriteProtected),
            0x0117 => Ok(Self::BlockUpdateTooLarge),
            0x0118 => Ok(Self::BlockNumberInvalid),
            0x0119 => Ok(Self::InvalidPassword),
            0x011A => Ok(Self::PGResourceError),
            0x011B => Ok(Self::PLCResourceError),
            0x011C => Ok(Self::ProtocolError),
            0x011D => Ok(Self::TooManyBlock),
            0x011E => Ok(Self::SessionExpired),
            0x011F => Ok(Self::ResultBufferTooSmall),
            0x0120 => Ok(Self::BlockEndList),
            0x0140 => Ok(Self::InsufficientAvailableMemory),
            0x0141 => Ok(Self::JobNotProcessed),
            0x8001 => Ok(Self::ServiceNotAllowedInCurrentState),
            0x8003 => Ok(Self::BlockTransferProtocolError),
            0x8100 => Ok(Self::UnknownServiceOnRemoteModule),
            0x8104 => Ok(Self::ServiceNotImplementedOrFrameError),
            0x8204 => Ok(Self::ObjectTypeMismatch),
            0x8205 => Ok(Self::CopiedBlockExistsUnlinked),
            0x8301 => Ok(Self::MemoryOrStorageUnavailable),
            0x8302 => Ok(Self::ResourcesInsufficientOrCpuBusy),
            0x8304 => Ok(Self::ParallelUploadNotPossible),
            0x8305 => Ok(Self::FunctionUnavailable),
            0x8306 => Ok(Self::WorkMemoryInsufficient),
            0x8307 => Ok(Self::RetentiveMemoryInsufficient),
            0x8401 => Ok(Self::InvalidServiceSequence),
            0x8402 => Ok(Self::ServiceBlockedByObjectState),
            0x8404 => Ok(Self::FunctionCannotBeExecuted),
            0x8405 => Ok(Self::RemoteBlockDisabled),
            0x8500 => Ok(Self::FrameError),
            0x8503 => Ok(Self::ServiceAbortedEarly),
            0x8701 => Ok(Self::ObjectAddressingError),
            0x8702 => Ok(Self::ServiceNotSupportedByModule),
            0x8703 => Ok(Self::ObjectAccessDenied),
            0x8704 => Ok(Self::ObjectCorrupted),
            0xD001 => Ok(Self::IllegalJobNumber),
            0xD002 => Ok(Self::IllegalJobVariant),
            0xD003 => Ok(Self::DebugNotSupported),
            0xD004 => Ok(Self::IllegalJobState),
            0xD005 => Ok(Self::IllegalJobAbort),
            0xD006 => Ok(Self::IllegalDisconnectId),
            0xD007 => Ok(Self::IllegalBufferElementCount),
            0xD008 => Ok(Self::IllegalScanRate),
            0xD009 => Ok(Self::IllegalExecutionCount),
            0xD00A => Ok(Self::IllegalTriggerEvent),
            0xD00B => Ok(Self::IllegalTriggerCondition),
            0xD011 => Ok(Self::BlockNotExistInPath),
            0xD012 => Ok(Self::AddressErrorInBlock),
            0xD014 => Ok(Self::DeletingOrOverwritingBlock),
            0xD015 => Ok(Self::IllegalTagAddress),
            0xD016 => Ok(Self::TestJobNotPossibleUserProgramError),
            0xD017 => Ok(Self::IllegalTriggerNumber),
            0xD025 => Ok(Self::InvalidPath),
            0xD026 => Ok(Self::IllegalAccessType),
            0xD027 => Ok(Self::TooManyDataBlocks),
            0xD031 => Ok(Self::InternalProtocolError),
            0xD032 => Ok(Self::ResultBufferLengthError),
            0xD033 => Ok(Self::JobLengthError),
            0xD03F => Ok(Self::ParamSectionEncodingError),
            0xD041 => Ok(Self::IllegalStateListId),
            0xD042 => Ok(Self::IllegalTagAddressData),
            0xD043 => Ok(Self::ReferencedJobMissing),
            0xD044 => Ok(Self::IllegalTagValue),
            0xD045 => Ok(Self::ODISExitNotAllowedInHold),
            0xD046 => Ok(Self::IllegalMeasurementPhase),
            0xD047 => Ok(Self::IllegalHierarchyInReadJobList),
            0xD048 => Ok(Self::IllegalDeleteId),
            0xD049 => Ok(Self::InvalidReplaceId),
            0xD04A => Ok(Self::ProgramStatusError),
            0xD05F => Ok(Self::DataSectionEncodingError),
            0xD061 => Ok(Self::NoMemoryForJob),
            0xD062 => Ok(Self::JobListFull),
            0xD063 => Ok(Self::TriggerEventOccupied),
            0xD064 => Ok(Self::NoMemoryForResultElement),
            0xD065 => Ok(Self::NoMemoryForResultElements),
            0xD066 => Ok(Self::TimerOccupiedByAnotherJob),
            0xD067 => Ok(Self::TooManyModifyJobs),
            0xD081 => Ok(Self::FunctionNotAllowedInCurrentMode),
            0xD082 => Ok(Self::CannotExitHoldMode),
            0xD0A1 => Ok(Self::FunctionNotAllowedAtProtectionLevel),
            0xD0A2 => Ok(Self::BusyDueToMemoryModifyingFunction),
            0xD0A3 => Ok(Self::TooManyModifyJobsOnIO),
            0xD0A4 => Ok(Self::ForcingAlreadyActive),
            0xD0A5 => Ok(Self::ReferencedJobNotFound),
            0xD0A6 => Ok(Self::CannotDisableEnableJob),
            0xD0A7 => Ok(Self::CannotDeleteJob),
            0xD0A8 => Ok(Self::CannotReplaceJob),
            0xD0A9 => Ok(Self::CannotReadJob),
            0xD0AA => Ok(Self::ProcessingTimeout),
            0xD0AB => Ok(Self::InvalidProcessJobParams),
            0xD0AC => Ok(Self::InvalidProcessJobData),
            0xD0AD => Ok(Self::OperationModeSet),
            0xD0AE => Ok(Self::JobBoundToAnotherConnection),
            0xD0C1 => Ok(Self::TagAccessErrorDetected),
            0xD0C2 => Ok(Self::SwitchedToStopOrHold),
            0xD0C3 => Ok(Self::TagAccessErrorThenStopOrHold),
            0xD0C4 => Ok(Self::RuntimeMeasurementTimeout),
            0xD0C5 => Ok(Self::BlockStackInconsistent),
            0xD0C6 => Ok(Self::JobDeletedDueToReferencedJobDeleted),
            0xD0C7 => Ok(Self::JobAutoDeletedDueToStopExit),
            0xD0C8 => Ok(Self::BlockStatusAbortedDueToInconsistency),
            0xD0C9 => Ok(Self::ExitStateAreaByResetOB90),
            0xD0CA => Ok(Self::ExitStateAreaByResetOB90AndReadError),
            0xD0CB => Ok(Self::PeripheralOutputDisableReactivated),
            0xD0CC => Ok(Self::DebugDataVolumeTimeLimited),
            0xD201 => Ok(Self::SyntaxErrorInBlockName),
            0xD202 => Ok(Self::SyntaxErrorInFunctionParams),
            0xD205 => Ok(Self::LinkedBlockExistsInRAM),
            0xD206 => Ok(Self::LinkedBlockExistsInEPROM),
            0xD208 => Ok(Self::MaxUnlinkedBlocksExceeded),
            0xD209 => Ok(Self::GivenBlockNotFoundOnModule),
            0xD20A => Ok(Self::MaxBlocksLinkedPerJobExceeded),
            0xD20B => Ok(Self::MaxBlocksDeletablePerJobExceeded),
            0xD20C => Ok(Self::OBPriorityMissing),
            0xD20D => Ok(Self::SDBNotInterpretable),
            0xD20E => Ok(Self::NoMoreBlocksAvailable),
            0xD20F => Ok(Self::ModuleSpecificMaxBlockSizeExceeded),
            0xD210 => Ok(Self::InvalidBlockNumberD210),
            0xD212 => Ok(Self::HeaderAttributesIncorrect),
            0xD213 => Ok(Self::TooManySDBs),
            0xD216 => Ok(Self::InvalidUserProgramResetModule),
            0xD217 => Ok(Self::ProtectionLevelNotAllowed),
            0xD218 => Ok(Self::IncorrectAttributeActivePassive),
            0xD219 => Ok(Self::IncorrectBlockLength),
            0xD21A => Ok(Self::IncorrectLocalDataLengthOrWriteProtect),
            0xD21B => Ok(Self::ModuleCannotCompressOrCompressionInterrupted),
            0xD21D => Ok(Self::IllegalDynamicProjectDataSize),
            0xD21E => Ok(Self::CannotAssignParametersToModule),
            0xD220 => Ok(Self::InvalidProgrammingLanguage),
            0xD221 => Ok(Self::InvalidConnectionOrRoutingSystemData),
            0xD222 => Ok(Self::InvalidGlobalDataSystemParams),
            0xD223 => Ok(Self::CommFBInstanceDbErrorOrTooManyBackgroundDb),
            0xD224 => Ok(Self::ScanSDBInvalidParams),
            0xD225 => Ok(Self::DpSDBInvalidParams),
            0xD226 => Ok(Self::StructuralErrorInBlock),
            0xD230 => Ok(Self::StructuralErrorInBlock2),
            0xD231 => Ok(Self::OBPriorityMissingOnCopy),
            0xD232 => Ok(Self::IllegalBlockNumberInLoadedBlock),
            0xD234 => Ok(Self::BlockExistsTwice),
            0xD235 => Ok(Self::BlockChecksumIncorrect),
            0xD236 => Ok(Self::BlockNoChecksum),
            0xD237 => Ok(Self::BlockAlreadyLoadedSameTimestamp),
            0xD238 => Ok(Self::SpecifiedBlockNotDB),
            0xD239 => Ok(Self::SpecifiedDBNotAvailableAsLinkVar),
            0xD23A => Ok(Self::SpecifiedDBDiffersTooMuch),
            0xD240 => Ok(Self::CoordinationRulesViolated),
            0xD241 => Ok(Self::FunctionNotAllowedAtCurrentProtectionLevel),
            0xD242 => Ok(Self::ProtectionConflictInFBlock),
            0xD250 => Ok(Self::UpdateAndModuleIdOrVersionMismatch),
            0xD251 => Ok(Self::OSComponentsSequenceIncorrect),
            0xD252 => Ok(Self::ChecksumError),
            0xD253 => Ok(Self::NoExecutableLoaderAvailable),
            0xD254 => Ok(Self::StorageErrorInOS),
            0xD280 => Ok(Self::CompileErrorOnS7300),
            0xD2A1 => Ok(Self::AnotherBlockFunctionOrTriggerActive),
            0xD2A2 => Ok(Self::TriggerActiveFinishDebugFirst),
            0xD2A3 => Ok(Self::BlockNotActivatedOrBusyOrMarkedForDelete),
            0xD2A4 => Ok(Self::BlockProcessedByAnotherFunction),
            0xD2A6 => Ok(Self::CannotSaveAndChangeProgramSimultaneously),
            0xD2A7 => Ok(Self::BlockUnlinkedOrNotProcessed),
            0xD2A8 => Ok(Self::ActiveDebugPreventsCpuParameterAssignment),
            0xD2A9 => Ok(Self::AssigningNewParamsToCpu),
            0xD2AA => Ok(Self::AssigningNewParamsToModule),
            0xD2AB => Ok(Self::ChangingDynamicConfigLimits),
            0xD2AC => Ok(Self::SFC12ActivationBlocksRKiR),
            0xD2B0 => Ok(Self::ErrorConfiguringInRunCiR),
            0xD2C0 => Ok(Self::MaxProcessObjectsExceeded),
            0xD2C1 => Ok(Self::SameTechnologyDbExists),
            0xD2C2 => Ok(Self::CannotDownloadUserProgramOrHardwareConfig),
            0xD401 => Ok(Self::InfoFunctionNotAvailable),
            0xD402 => Ok(Self::InfoFunctionNotAvailable2),
            0xD403 => Ok(Self::ServiceLoggedInOut),
            0xD404 => Ok(Self::MaxNodesReached),
            0xD405 => Ok(Self::ServiceNotSupportedOrSyntaxErrorInParams),
            0xD406 => Ok(Self::RequiredInfoCurrentlyUnavailable),
            0xD407 => Ok(Self::DiagnosticErrorOccurred),
            0xD408 => Ok(Self::UpdateAborted),
            0xD409 => Ok(Self::DPBusError),
            0xD601 => Ok(Self::SyntaxErrorInFunctionParamsD601),
            0xD602 => Ok(Self::IncorrectPassword),
            0xD603 => Ok(Self::ConnectionLegalized),
            0xD604 => Ok(Self::ConnectionEnabled),
            0xD605 => Ok(Self::LegalizationNotPossiblePasswordMissing),
            0xD801 => Ok(Self::AtLeastOneTagAddressInvalid),
            0xD802 => Ok(Self::SpecifiedJobDoesNotExist),
            0xD803 => Ok(Self::IllegalOperatingState),
            0xD804 => Ok(Self::IllegalCycleTime),
            0xD805 => Ok(Self::NoMoreCyclicReadJobs),
            0xD806 => Ok(Self::ReferencedJobInWrongState),
            0xD807 => Ok(Self::FunctionAbortedDueToOverload),
            0xDC01 => Ok(Self::InvalidDateOrTime),
            0xE201 => Ok(Self::CpuAlreadyMaster),
            0xE202 => Ok(Self::CannotConnectUpdateDueToDifferentUserProgram),
            0xE203 => Ok(Self::CannotConnectUpdateDueToDifferentFirmware),
            0xE204 => Ok(Self::CannotConnectUpdateDueToDifferentMemoryConfig),
            0xE205 => Ok(Self::CannotConnectUpdateDueToSynchronization),
            0xE206 => Ok(Self::CannotConnectUpdateDueToCoordinationViolation),
            0xEF01 => Ok(Self::ErrorAtId2Only00HPermittedInJob),
            0xEF02 => Ok(Self::ErrorAtId2SetOfResourcesDoesNotExist),
            _ => Err(()),
        }
    }
}
